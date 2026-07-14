//! The checks themselves.
//!
//! [`check_run_set`] loads a run-set from a directory and returns a
//! [`CheckReport`] — a fixed-order list of per-check [`Outcome`]s. The report's
//! ordering is deterministic (the checks run in a fixed sequence, and every detail
//! that scans records reports sample ids in sorted order), because the checker's
//! own output is itself retained evidence: `docs/ARM-ALTRA.md` §Evidence integrity
//! requires it to be reproducible, so no wall-clock, no iteration-order, and no
//! hashing of a `HashMap` may reach a byte of it.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

/// The conventional manifest file name inside a run-set directory.
pub const MANIFEST_FILE: &str = "run-set.json";

use arm_harness::evidence::{ExitReason, RunRecord, RunSet, SCHEMA_VERSION, Stage};
use oracle_model::{Weights, expected};
use sha2::{Digest, Sha256};

use crate::error::LoadError;

/// Which floors the caller asked the checker to enforce.
///
/// Absent means "not requested" — a floor the caller did not name is not a
/// silent pass of an unmet requirement, it is simply not part of *this*
/// invocation's question. The real acceptance floors (≥10⁶ armed overflows for
/// AA-1/AA-3, ≥1,000 reps for AA-6) are passed explicitly on the command line so
/// the number a disposition rests on is visible in the command that produced the
/// verdict, never buried as a default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Floors {
    /// `--min-armed-overflows`: the run-set must contain at least this many armed
    /// overflows, counted by scanning the records.
    pub min_armed_overflows: Option<u64>,
    /// `--min-reps`: the run-set must contain at least this many samples (AA-6's
    /// same-seed repetition floor).
    pub min_reps: Option<u64>,
}

/// The identity of a single check. Also its stable name, via [`CheckId::name`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckId {
    /// The manifest's `schema_version` is one this checker understands.
    SchemaVersion,
    /// sha256 of the records file equals the manifest's `records_sha256`.
    RecordsSha256,
    /// The sample ids are exactly `0..attempted`, each present once.
    Totality,
    /// Every armed overflow was delivered exactly once.
    Multiplicity,
    /// The manifest carries measured weights (else counts cannot be checked).
    WeightsPresent,
    /// Every record's count matches the oracle, and its own `measured_taken`
    /// matches `work_end - work_begin`.
    CountExactness,
    /// The manifest carries a measured skid margin (else skid cannot be bounded).
    SkidMarginPresent,
    /// No landing overshot; every skid is within margin; AA-3 landings are exact.
    Skid,
    /// Every record's exit reason matches the claimed mechanism, and a patched
    /// claim was positively observed.
    MechanismAttestation,
    /// Every boot artifact was content-verified immediately before use.
    ImagePins,
    /// The vCPU was pinned (unless this is AA-1's sanctioned migration probe).
    Pinning,
    /// Every record attests the guest ran in `managed` params mode.
    ParamsMode,
    /// Every payload's in-guest self-checks passed (`payload_status == 0`).
    PayloadStatus,
    /// The armed-overflow count meets `--min-armed-overflows`.
    ArmedOverflowFloor,
    /// The sample count meets `--min-reps`.
    RepFloor,
}

impl CheckId {
    /// The check's stable, kebab-case name — the identifier that appears in the
    /// checker's retained output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CheckId::SchemaVersion => "schema-version",
            CheckId::RecordsSha256 => "records-sha256",
            CheckId::Totality => "totality",
            CheckId::Multiplicity => "multiplicity",
            CheckId::WeightsPresent => "weights-present",
            CheckId::CountExactness => "count-exactness",
            CheckId::SkidMarginPresent => "skid-margin-present",
            CheckId::Skid => "skid",
            CheckId::MechanismAttestation => "mechanism-attestation",
            CheckId::ImagePins => "image-pins",
            CheckId::Pinning => "pinning",
            CheckId::ParamsMode => "params-mode",
            CheckId::PayloadStatus => "payload-status",
            CheckId::ArmedOverflowFloor => "armed-overflow-floor",
            CheckId::RepFloor => "rep-floor",
        }
    }
}

impl fmt::Display for CheckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A check's verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// The check held.
    Pass,
    /// The check failed — the run-set may not be dispositioned on.
    Fail,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Pass => f.write_str("PASS"),
            Status::Fail => f.write_str("FAIL"),
        }
    }
}

/// One check's result, with the detail that makes a failure diagnosable.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// Which check.
    pub id: CheckId,
    /// Its verdict.
    pub status: Status,
    /// A one-line human explanation. Always present on a failure; on a pass it is
    /// a short affirmative so the retained output reads as a positive record.
    pub detail: String,
}

/// The full verdict: one [`Outcome`] per check, in a fixed order.
#[derive(Clone, Debug)]
pub struct CheckReport {
    /// The run-set's identifier, echoed for the retained record.
    pub run_set_id: String,
    /// The stage the run-set claims.
    pub stage: Stage,
    /// The per-check outcomes, in a fixed, reproducible order.
    pub outcomes: Vec<Outcome>,
}

impl CheckReport {
    /// Whether every check passed. The checker exits 0 exactly when this is true.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.status == Status::Pass)
    }

    /// The process exit code: 0 iff every check passed.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.passed())
    }

    /// The status of a given check, if it ran in this invocation. Floors that were
    /// not requested (`--min-*` absent) do not appear.
    #[must_use]
    pub fn status_of(&self, id: CheckId) -> Option<Status> {
        self.outcomes
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.status)
    }

    /// The ids of the checks that failed, in report order.
    #[must_use]
    pub fn failed(&self) -> Vec<CheckId> {
        self.outcomes
            .iter()
            .filter(|o| o.status == Status::Fail)
            .map(|o| o.id)
            .collect()
    }
}

/// Load a run-set from a directory and check it.
///
/// The directory must contain a `run-set.json` manifest; the manifest names its
/// own records file (conventionally `records.jsonl`), resolved relative to the
/// directory.
///
/// # Errors
///
/// Returns [`LoadError`] only when the evidence is *unreadable* — a missing or
/// malformed manifest or records file. A run-set that loads but fails a floor is
/// not an error; it is a [`CheckReport`] with failing outcomes and a nonzero
/// [`CheckReport::exit_code`].
pub fn check_run_set(dir: &Path, floors: &Floors) -> Result<CheckReport, LoadError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let manifest_bytes =
        std::fs::read(&manifest_path).map_err(|source| LoadError::ReadManifest {
            path: manifest_path.clone(),
            source,
        })?;
    let run_set: RunSet =
        serde_json::from_slice(&manifest_bytes).map_err(|source| LoadError::ParseManifest {
            path: manifest_path.clone(),
            source,
        })?;

    let records_path = dir.join(&run_set.records_file);
    let records_bytes = std::fs::read(&records_path).map_err(|source| LoadError::ReadRecords {
        path: records_path.clone(),
        source,
    })?;
    let records = parse_records(&records_path, &records_bytes)?;

    let mut outcomes = Vec::new();

    check_schema_version(&run_set, &mut outcomes);
    check_records_sha256(&run_set, &records_bytes, &mut outcomes);
    check_totality(&run_set, &records, &mut outcomes);
    check_multiplicity(&records, &mut outcomes);
    check_weights_and_counts(&run_set, &records, &mut outcomes);
    check_skid(&run_set, &records, &mut outcomes);
    check_mechanism(&run_set, &records, &mut outcomes);
    check_image_pins(&run_set, &mut outcomes);
    check_pinning(&run_set, &mut outcomes);
    check_params_mode(&records, &mut outcomes);
    check_payload_status(&records, &mut outcomes);
    check_floors(floors, &records, &mut outcomes);

    Ok(CheckReport {
        run_set_id: run_set.run_set_id,
        stage: run_set.stage,
        outcomes,
    })
}

/// Parse `records.jsonl`: one [`RunRecord`] per non-empty line.
fn parse_records(path: &Path, bytes: &[u8]) -> Result<Vec<RunRecord>, LoadError> {
    let text = std::str::from_utf8(bytes).map_err(|e| LoadError::ReadRecords {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    let mut records = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: RunRecord =
            serde_json::from_str(line).map_err(|source| LoadError::ParseRecord {
                path: path.to_path_buf(),
                line: idx + 1,
                source,
            })?;
        records.push(record);
    }
    Ok(records)
}

fn pass(id: CheckId, detail: impl Into<String>) -> Outcome {
    Outcome {
        id,
        status: Status::Pass,
        detail: detail.into(),
    }
}

fn fail(id: CheckId, detail: impl Into<String>) -> Outcome {
    Outcome {
        id,
        status: Status::Fail,
        detail: detail.into(),
    }
}

fn check_schema_version(run_set: &RunSet, out: &mut Vec<Outcome>) {
    if run_set.schema_version == SCHEMA_VERSION {
        out.push(pass(
            CheckId::SchemaVersion,
            format!("schema version {SCHEMA_VERSION}"),
        ));
    } else {
        out.push(fail(
            CheckId::SchemaVersion,
            format!(
                "unknown schema version {} (this checker knows {SCHEMA_VERSION}); \
                 refusing to guess at the bytes",
                run_set.schema_version
            ),
        ));
    }
}

/// Lowercase-hex-encode a byte slice. No `hex` crate is on the whitelist, and this
/// is the only place one is needed.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Normalise a recorded hash: drop an optional `sha256:` prefix and lowercase it.
fn normalise_hash(h: &str) -> String {
    h.strip_prefix("sha256:").unwrap_or(h).to_ascii_lowercase()
}

fn check_records_sha256(run_set: &RunSet, records_bytes: &[u8], out: &mut Vec<Outcome>) {
    let mut hasher = Sha256::new();
    hasher.update(records_bytes);
    let computed = hex_lower(&hasher.finalize());
    let claimed = normalise_hash(&run_set.records_sha256);
    if computed == claimed {
        out.push(pass(
            CheckId::RecordsSha256,
            format!("records sha256 {computed} matches the manifest"),
        ));
    } else {
        out.push(fail(
            CheckId::RecordsSha256,
            format!(
                "records file sha256 is {computed} but the manifest pins {claimed}: \
                 the records were swapped, truncated, or edited"
            ),
        ));
    }
}

fn check_totality(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let attempted = run_set.attempted;
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut duplicates: BTreeSet<u64> = BTreeSet::new();
    let mut out_of_range: BTreeSet<u64> = BTreeSet::new();

    for r in records {
        if r.sample_id >= attempted {
            out_of_range.insert(r.sample_id);
        }
        if !seen.insert(r.sample_id) {
            duplicates.insert(r.sample_id);
        }
    }

    // The gaps are the ids in 0..attempted that never appeared.
    let missing: Vec<u64> = (0..attempted).filter(|id| !seen.contains(id)).collect();

    if duplicates.is_empty() && out_of_range.is_empty() && missing.is_empty() {
        out.push(pass(
            CheckId::Totality,
            format!("all {attempted} attempted samples present exactly once"),
        ));
        return;
    }

    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!(
            "missing sample ids {} (a missing sample is a failure to account, not a pass)",
            preview(missing.iter().copied())
        ));
    }
    if !duplicates.is_empty() {
        problems.push(format!(
            "duplicate sample ids {}",
            preview(duplicates.iter().copied())
        ));
    }
    if !out_of_range.is_empty() {
        problems.push(format!(
            "sample ids outside 0..{attempted}: {}",
            preview(out_of_range.iter().copied())
        ));
    }
    out.push(fail(CheckId::Totality, problems.join("; ")));
}

fn check_multiplicity(records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut lost: Vec<u64> = Vec::new();
    let mut duplicated: Vec<u64> = Vec::new();
    let mut armed = 0u64;

    for r in records {
        if let Some(o) = &r.overflow {
            if o.armed {
                armed += 1;
                match o.deliveries {
                    1 => {}
                    0 => lost.push(r.sample_id),
                    _ => duplicated.push(r.sample_id),
                }
            }
        }
    }
    lost.sort_unstable();
    duplicated.sort_unstable();

    if lost.is_empty() && duplicated.is_empty() {
        out.push(pass(
            CheckId::Multiplicity,
            format!("all {armed} armed overflows delivered exactly once"),
        ));
        return;
    }

    let mut problems = Vec::new();
    if !lost.is_empty() {
        problems.push(format!(
            "lost PMIs (deliveries == 0) at samples {}",
            preview(lost.iter().copied())
        ));
    }
    if !duplicated.is_empty() {
        problems.push(format!(
            "duplicate deliveries (> 1) at samples {}",
            preview(duplicated.iter().copied())
        ));
    }
    out.push(fail(CheckId::Multiplicity, problems.join("; ")));
}

/// The weights-present and count-exactness checks travel together: without
/// measured weights, the count check is *refused* (never defaulted), and that
/// refusal is itself a failing outcome.
fn check_weights_and_counts(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let Some(weights) = run_set.weights else {
        out.push(fail(
            CheckId::WeightsPresent,
            "the manifest carries no measured weights (`weights: null`): \
             refusing to check counts rather than substituting a default \
             (task 109: count offsets are spike deliverables, never defaults)",
        ));
        out.push(fail(
            CheckId::CountExactness,
            "not checked: counts cannot be recomputed without measured weights",
        ));
        return;
    };
    out.push(pass(
        CheckId::WeightsPresent,
        "the manifest carries measured weights",
    ));
    check_counts(&weights, records, out);
}

fn check_counts(weights: &Weights, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut problems: Vec<String> = Vec::new();

    for r in records {
        // Recompute the measured count from the two window endpoints, and fail the
        // record's own `measured_taken` if it disagrees.
        match r.work_end.checked_sub(r.work_begin) {
            Some(delta) if delta == r.measured_taken => {}
            Some(delta) => problems.push(format!(
                "sample {}: measured_taken {} != work_end - work_begin ({delta})",
                r.sample_id, r.measured_taken
            )),
            None => problems.push(format!(
                "sample {}: work_end {} is before work_begin {} (negative window)",
                r.sample_id, r.work_end, r.work_begin
            )),
        }

        // The oracle is only defined for payloads that have a counting window.
        if !r.payload.has_window() {
            continue;
        }

        let e = expected(r.payload, r.scale, r.seed);

        // A payload with no reported term may not report retries: a nonzero value
        // would silently inflate the prediction to match a corrupt measurement.
        if !e.has_reported_term && r.reported_taken != 0 {
            problems.push(format!(
                "sample {}: payload {} has no reported term but reported_taken is {}",
                r.sample_id,
                r.payload.name(),
                r.reported_taken
            ));
        }

        let predicted = e.total(weights, r.reported_taken);
        if predicted != r.measured_taken {
            problems.push(format!(
                "sample {}: payload {} scale {} seed {}: oracle predicts {predicted} \
                 taken branches but the record measured {}",
                r.sample_id,
                r.payload.name(),
                r.scale.name(),
                r.seed,
                r.measured_taken
            ));
        }
    }

    if problems.is_empty() {
        out.push(pass(
            CheckId::CountExactness,
            format!(
                "all {} records match the oracle and are self-consistent",
                records.len()
            ),
        ));
    } else {
        out.push(fail(CheckId::CountExactness, join_problems(&problems)));
    }
}

/// Skid: never overshoot, always within margin, and — at AA-3 — land exactly.
///
/// The margin check is refused (never defaulted) when the manifest carries no
/// measured margin, mirroring the weights refusal.
fn check_skid(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let margin = run_set.skid_margin;
    if margin.is_some() {
        out.push(pass(
            CheckId::SkidMarginPresent,
            "the manifest carries a measured skid margin",
        ));
    } else {
        out.push(fail(
            CheckId::SkidMarginPresent,
            "the manifest carries no measured skid margin (`skid_margin: null`): \
             skid cannot be bounded, and the checker refuses to invent one",
        ));
    }

    let exact_required = run_set.stage == Stage::Aa3;
    let mut problems: Vec<String> = Vec::new();

    for r in records {
        let Some(o) = &r.overflow else { continue };
        if !o.armed {
            continue;
        }

        // Recompute the skid from landed and target; fail the record's own field
        // if it disagrees.
        let recomputed = i128::from(o.landed) - i128::from(o.target);
        if recomputed != i128::from(o.skid) {
            problems.push(format!(
                "sample {}: skid field {} != landed - target ({recomputed})",
                r.sample_id, o.skid
            ));
        }

        // Never overshoot: a positive skid violates the late-only-stop contract
        // outright, whatever the margin.
        if recomputed > 0 {
            problems.push(format!(
                "sample {}: landing overshot the target by {recomputed} \
                 (landed {} > target {}); the late-only-stop contract forbids it",
                r.sample_id, o.landed, o.target
            ));
        }

        // Within margin, when a margin was measured.
        if let Some(m) = margin {
            if recomputed.unsigned_abs() > u128::from(m) {
                problems.push(format!(
                    "sample {}: |skid| {} exceeds the measured margin {m}",
                    r.sample_id,
                    recomputed.unsigned_abs()
                ));
            }
        }

        // AA-3's exact landing: work == target on every landing.
        if exact_required && recomputed != 0 {
            problems.push(format!(
                "sample {}: AA-3 requires work == target but landed {} != target {}",
                r.sample_id, o.landed, o.target
            ));
        }
    }

    if problems.is_empty() {
        out.push(pass(
            CheckId::Skid,
            if exact_required {
                "no overshoot; all landings within margin and exact (AA-3)"
            } else {
                "no overshoot; all landings within margin"
            },
        ));
    } else {
        out.push(fail(CheckId::Skid, join_problems(&problems)));
    }
}

fn check_mechanism(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let m = &run_set.mechanism;
    let mut problems: Vec<String> = Vec::new();

    // A patched claim must have been positively observed, not assumed from a build.
    if m.kvm_patched && !m.patch_marker_observed {
        problems.push(
            "manifest claims kvm_patched but patch_marker_observed is false: \
             a patched claim must be positively observed in the running kernel"
                .to_string(),
        );
    }

    // Every record's exit reason must be the one the manifest claims. This is the
    // PR-98 lesson: a run that silently exercised the stock signal-kick path while
    // claiming the patched Preempt exit must fail here.
    let mut mismatched: Vec<(u64, ExitReason)> = Vec::new();
    for r in records {
        if r.exit_reason != m.expected_exit_reason {
            mismatched.push((r.sample_id, r.exit_reason));
        }
    }
    if !mismatched.is_empty() {
        mismatched.sort_by_key(|&(id, _)| id);
        let shown: Vec<String> = mismatched
            .iter()
            .take(8)
            .map(|(id, er)| format!("sample {id}={er:?}"))
            .collect();
        let suffix = if mismatched.len() > 8 {
            format!(" (+{} more)", mismatched.len() - 8)
        } else {
            String::new()
        };
        problems.push(format!(
            "{} record(s) carry an exit reason other than the claimed {:?}: {}{suffix}",
            mismatched.len(),
            m.expected_exit_reason,
            shown.join(", ")
        ));
    }

    if problems.is_empty() {
        out.push(pass(
            CheckId::MechanismAttestation,
            format!(
                "all records carry the claimed {:?} exit; mechanism attested",
                m.expected_exit_reason
            ),
        ));
    } else {
        out.push(fail(CheckId::MechanismAttestation, problems.join("; ")));
    }
}

fn check_image_pins(run_set: &RunSet, out: &mut Vec<Outcome>) {
    let unverified: Vec<&str> = run_set
        .images
        .iter()
        .filter(|i| !i.verified_before_boot)
        .map(|i| i.path.as_str())
        .collect();
    if run_set.images.is_empty() {
        out.push(fail(
            CheckId::ImagePins,
            "the manifest pins no boot artifacts: nothing was attested",
        ));
    } else if unverified.is_empty() {
        out.push(pass(
            CheckId::ImagePins,
            format!(
                "all {} boot artifacts verified before boot",
                run_set.images.len()
            ),
        ));
    } else {
        out.push(fail(
            CheckId::ImagePins,
            format!(
                "{} boot artifact(s) recorded a hash but were not verified before boot: {}",
                unverified.len(),
                unverified.join(", ")
            ),
        ));
    }
}

fn check_pinning(run_set: &RunSet, out: &mut Vec<Outcome>) {
    let p = &run_set.pinning;
    if p.pinned || p.migration_probe {
        let detail = if p.migration_probe {
            "unpinned, but marked as AA-1's sanctioned migration probe".to_string()
        } else {
            match p.core {
                Some(c) => format!("pinned to core {c}"),
                None => "pinned (core unrecorded)".to_string(),
            }
        };
        out.push(pass(CheckId::Pinning, detail));
    } else {
        out.push(fail(
            CheckId::Pinning,
            "the vCPU was not pinned and this is not the sanctioned migration probe: \
             on this lineage a missed PMI on migration (rr #3607) can wedge KVM_RUN",
        ));
    }
}

fn check_params_mode(records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut bad: Vec<u64> = records
        .iter()
        .filter(|r| r.params_mode != "managed")
        .map(|r| r.sample_id)
        .collect();
    bad.sort_unstable();
    if bad.is_empty() {
        out.push(pass(
            CheckId::ParamsMode,
            "every record attests managed params mode",
        ));
    } else {
        out.push(fail(
            CheckId::ParamsMode,
            format!(
                "{} record(s) did not run in managed params mode: samples {} \
                 (an unpublished params page runs the smoke scale under a 1e8 claim)",
                bad.len(),
                preview(bad.iter().copied())
            ),
        ));
    }
}

fn check_payload_status(records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut bad: Vec<(u64, i32)> = records
        .iter()
        .filter(|r| r.payload_status != 0)
        .map(|r| (r.sample_id, r.payload_status))
        .collect();
    bad.sort_by_key(|&(id, _)| id);
    if bad.is_empty() {
        out.push(pass(
            CheckId::PayloadStatus,
            "every payload's in-guest self-checks passed",
        ));
    } else {
        let shown: Vec<String> = bad
            .iter()
            .take(8)
            .map(|(id, s)| format!("sample {id}={s}"))
            .collect();
        out.push(fail(
            CheckId::PayloadStatus,
            format!(
                "{} record(s) report a nonzero payload_status: {}",
                bad.len(),
                shown.join(", ")
            ),
        ));
    }
}

fn check_floors(floors: &Floors, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if let Some(min) = floors.min_armed_overflows {
        let armed = records
            .iter()
            .filter(|r| r.overflow.as_ref().is_some_and(|o| o.armed))
            .count() as u64;
        if armed >= min {
            out.push(pass(
                CheckId::ArmedOverflowFloor,
                format!("{armed} armed overflows meets the floor of {min}"),
            ));
        } else {
            out.push(fail(
                CheckId::ArmedOverflowFloor,
                format!("only {armed} armed overflows, below the floor of {min}"),
            ));
        }
    }

    if let Some(min) = floors.min_reps {
        let reps = records.len() as u64;
        if reps >= min {
            out.push(pass(
                CheckId::RepFloor,
                format!("{reps} samples meets the rep floor of {min}"),
            ));
        } else {
            out.push(fail(
                CheckId::RepFloor,
                format!("only {reps} samples, below the rep floor of {min}"),
            ));
        }
    }
}

/// Render up to eight ids, then a count of the remainder, so a failure detail
/// stays bounded and deterministic on a run-set with many bad samples.
fn preview(ids: impl Iterator<Item = u64>) -> String {
    let all: Vec<u64> = ids.collect();
    let shown: Vec<String> = all.iter().take(8).map(u64::to_string).collect();
    if all.len() > 8 {
        format!("[{}, +{} more]", shown.join(", "), all.len() - 8)
    } else {
        format!("[{}]", shown.join(", "))
    }
}

/// Join per-record problems into one bounded, deterministic detail line.
fn join_problems(problems: &[String]) -> String {
    let shown: Vec<String> = problems.iter().take(8).cloned().collect();
    if problems.len() > 8 {
        format!("{} (+{} more)", shown.join("; "), problems.len() - 8)
    } else {
        shown.join("; ")
    }
}
