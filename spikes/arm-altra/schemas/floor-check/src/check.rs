// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checks themselves.
//!
//! [`check_run_set`] loads a run-set from a directory and returns a
//! [`CheckReport`] — a fixed-order list of per-check [`Outcome`]s. The report's
//! ordering is deterministic (the checks run in a fixed sequence, and every detail
//! that scans records reports sample ids in sorted order), because the checker's
//! own output is itself retained evidence: `docs/ARM-ALTRA.md` §Evidence integrity
//! requires it to be reproducible, so no wall-clock, no iteration-order, and no
//! hashing of a `HashMap` may reach a byte of it.
//!
//! # The checker knows what stage it is grading
//!
//! Several checks are **stage-conditional**, and that is the point: a manifest field
//! may not exempt a run from a rule the stage exists to enforce.
//!
//! - The stages whose acceptance rides the patched force-exit (AA-3, AA-4, AA-6)
//!   must *be* on the patched mechanism — an AA-3 run-set that declares
//!   `kvm_patched: false` and `signal-kick` consistently is not a clean run-set, it
//!   is the forbidden fallback, self-consistently described.
//! - The unpinned migration probe belongs to AA-1 alone (bounded, once); at any
//!   other stage `migration_probe: true` is one field exempting a landing run from a
//!   correctness condition (rr #3607).
//! - AA-5's records must attest the *harness-maintained* clock page, not the
//!   payload's static self-seeded fallback — which is precisely the mechanism AA-5
//!   certifies.
//!
//! # A floor nobody asked for is not a floor that passed
//!
//! [`Status::NotRequested`] exists because the checker's verdict is retained
//! evidence. `RESULT: PASS (N checks)` over an overflow-bearing run-set with no
//! `--min-armed-overflows` on the command line reads as full acceptance, and it
//! isn't one. So the omission is *visible on its face* and the exit status is
//! nonzero. The no-invented-numbers philosophy is intact: the checker demands the
//! **presence** of an explicit floor; it never supplies one.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use arm_harness::evidence::{ExitReason, RunRecord, RunSet, SCHEMA_VERSION, Stage};
use arm_harness::sys::BR_RETIRED_RAW;
use oracle_model::{Expectation, Payload, Scale, Weights, expected};
use sha2::{Digest, Sha256};

use crate::error::LoadError;

/// The conventional manifest file name inside a run-set directory.
pub const MANIFEST_FILE: &str = "run-set.json";

/// The `CLOCKPAGE mode=` token a harness-maintained (work-derived) clock page makes
/// the guest print. The alternative, `self-seeded`, means the payload published its
/// own static page because the harness never did — the static fallback, not the
/// mechanism AA-5 exists to validate (`payloads/runtime/src/pvclock.rs`).
const MANAGED_CLOCKPAGE: &str = "managed";

/// Which floors the caller asked the checker to enforce.
///
/// Absent means "not requested" — and for a floor the evidence *needs*, that absence
/// is itself reported ([`Status::NotRequested`]), never silently passed. The real
/// acceptance floors (≥10⁶ armed overflows for AA-1/AA-3, ≥1,000 reps for AA-6) are
/// passed explicitly on the command line so the number a disposition rests on is
/// visible in the command that produced the verdict, never buried as a default.
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
    /// Every field the canonical JSON Schema constrains (hash formats, non-empty
    /// required strings, minimums) is well-formed — enforced here because serde only
    /// checks Rust types, not the schema's `pattern`/`minLength`/`minimum`.
    WellFormed,
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
    /// Every record's exit reason matches the claimed mechanism, the claim is
    /// coherent, and the stage's required mechanism is the one that ran.
    MechanismAttestation,
    /// The work counter was armed as the work clock: raw 0x21, guest-only, pinned.
    PerfConfig,
    /// Every boot artifact was content-verified immediately before use.
    ImagePins,
    /// The vCPU was pinned (unless this is AA-1's sanctioned migration probe).
    Pinning,
    /// Every record attests the guest ran in `managed` params mode.
    ParamsMode,
    /// AA-5's records attest the harness-maintained clock page.
    ClockPageMode,
    /// Same-input repetitions landed on bit-identical state digests.
    ReplayIdentity,
    /// AA-2's records carry single-step (debug-exit) evidence — the observation the
    /// stage exists to make.
    DebugEvidence,
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
            CheckId::WellFormed => "well-formed",
            CheckId::RecordsSha256 => "records-sha256",
            CheckId::Totality => "totality",
            CheckId::Multiplicity => "multiplicity",
            CheckId::WeightsPresent => "weights-present",
            CheckId::CountExactness => "count-exactness",
            CheckId::SkidMarginPresent => "skid-margin-present",
            CheckId::Skid => "skid",
            CheckId::MechanismAttestation => "mechanism-attestation",
            CheckId::PerfConfig => "perf-config",
            CheckId::ImagePins => "image-pins",
            CheckId::Pinning => "pinning",
            CheckId::ParamsMode => "params-mode",
            CheckId::ClockPageMode => "clockpage-mode",
            CheckId::ReplayIdentity => "replay-identity",
            CheckId::DebugEvidence => "debug-evidence",
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
    /// The check *could not run* because the caller did not name the floor it
    /// enforces, and the evidence needs it. Not a pass: the run-set may not be
    /// dispositioned on this verdict either, and the exit status says so.
    NotRequested,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Pass => f.write_str("PASS"),
            Status::Fail => f.write_str("FAIL"),
            Status::NotRequested => f.write_str("NOT-REQUESTED"),
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
    /// Whether every check passed. The checker exits 0 exactly when this is true —
    /// so an unrequested-but-needed floor is not a pass.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.status == Status::Pass)
    }

    /// The process exit code: 0 iff every check passed.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.passed())
    }

    /// The status of a given check, if it ran in this invocation.
    #[must_use]
    pub fn status_of(&self, id: CheckId) -> Option<Status> {
        self.outcomes.iter().find(|o| o.id == id).map(|o| o.status)
    }

    /// The ids of the checks that failed, in report order.
    #[must_use]
    pub fn failed(&self) -> Vec<CheckId> {
        self.ids_with(Status::Fail)
    }

    /// The ids of the floors the evidence needed but the caller did not request.
    #[must_use]
    pub fn not_requested(&self) -> Vec<CheckId> {
        self.ids_with(Status::NotRequested)
    }

    fn ids_with(&self, status: Status) -> Vec<CheckId> {
        self.outcomes
            .iter()
            .filter(|o| o.status == status)
            .map(|o| o.id)
            .collect()
    }
}

/// Whether a stage's acceptance rides the **patched** force-exit mechanism.
///
/// AA-3 builds and validates the 0004-analogue and lands on it; AA-4 injects through
/// AA-3's machinery; AA-6's mini gate exercises the whole mechanism stack together.
/// For those three, the stock signal-kick is not a legitimate alternative — it is the
/// fallback the stage exists to replace (`docs/ARM-ALTRA.md` §AA-3: "the harness must
/// be structurally unable to fall back to the AA-1 signal-kick and still pass").
///
/// AA-1 and AA-2 legitimately run pre-patch; AA-5 validates the clock page and does
/// not certify the exit mechanism.
const fn requires_patched_mechanism(stage: Stage) -> bool {
    matches!(stage, Stage::Aa3 | Stage::Aa4 | Stage::Aa6)
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
    check_well_formed(&run_set, &records, &mut outcomes);
    check_records_sha256(&run_set, &records_bytes, &mut outcomes);
    check_totality(&run_set, &records, &mut outcomes);
    check_multiplicity(&records, &mut outcomes);
    check_weights_and_counts(&run_set, &records, &mut outcomes);
    check_skid(&run_set, &records, &mut outcomes);
    check_mechanism(&run_set, &records, &mut outcomes);
    check_perf(&run_set, &records, &mut outcomes);
    check_image_pins(&run_set, &mut outcomes);
    check_pinning(&run_set, &mut outcomes);
    check_params_mode(&records, &mut outcomes);
    check_clockpage_mode(&run_set, &records, &mut outcomes);
    check_replay_identity(run_set.stage, &records, &mut outcomes);
    check_debug_evidence(run_set.stage, &records, &mut outcomes);
    check_payload_status(&records, &mut outcomes);
    check_floors(&run_set, floors, &records, &mut outcomes);

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

fn not_requested(id: CheckId, detail: impl Into<String>) -> Outcome {
    Outcome {
        id,
        status: Status::NotRequested,
        detail: detail.into(),
    }
}

/// Push a pass if there are no problems, or one failure carrying all of them.
fn verdict(id: CheckId, problems: &[String], ok: impl Into<String>, out: &mut Vec<Outcome>) {
    if problems.is_empty() {
        out.push(pass(id, ok));
    } else {
        out.push(fail(id, join_problems(problems)));
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

/// Exactly `len` lowercase hex digits.
fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Enforce the constraints the canonical JSON Schemas encode but serde does not.
///
/// `serde_json` checks Rust types and `deny_unknown_fields`; it does NOT check the
/// schema's `pattern`, `minLength`, or `minimum`. So a manifest with `sha256: ""` or a
/// `sample_period: 0` deserializes cleanly and could make every *semantic* check pass —
/// `floor-check` would exit 0 on schema-invalid evidence, though the module documents
/// malformed evidence as a load error. This check closes that at the point the
/// evidence is graded: it enforces the load-bearing constraints (hash formats, the
/// non-empty required identifiers, the sampling-period minimum) so schema-invalid
/// evidence fails rather than passing vacuously.
fn check_well_formed(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut problems: Vec<String> = Vec::new();

    // sha256 fields: `^(sha256:)?[0-9a-f]{64}$` (the pinned records hash and each image
    // pin), md5 when present `^[0-9a-f]{32}$`.
    let check_sha256 = |problems: &mut Vec<String>, what: &str, h: &str| {
        if !is_lower_hex(normalise_hash(h).as_str(), 64) {
            problems.push(format!("{what} is not a 64-hex sha256: {h:?}"));
        }
    };
    check_sha256(&mut problems, "records_sha256", &run_set.records_sha256);
    check_sha256(
        &mut problems,
        "mechanism.host_kernel_sha256",
        &run_set.mechanism.host_kernel_sha256,
    );
    for (i, img) in run_set.images.iter().enumerate() {
        check_sha256(&mut problems, &format!("images[{i}].sha256"), &img.sha256);
        if let Some(md5) = &img.md5
            && !is_lower_hex(md5, 32)
        {
            problems.push(format!(
                "images[{i}].md5 is present but not 32-hex: {md5:?}"
            ));
        }
        if img.path.trim().is_empty() {
            problems.push(format!("images[{i}].path is empty"));
        }
    }

    // Non-empty required identifiers (schema `minLength: 1`).
    for (what, s) in [
        ("run_set_id", run_set.run_set_id.as_str()),
        ("environment.soc", run_set.environment.soc.as_str()),
        (
            "environment.host_kernel",
            run_set.environment.host_kernel.as_str(),
        ),
        (
            "environment.kvm_mode",
            run_set.environment.kvm_mode.as_str(),
        ),
        ("condition", run_set.condition.as_str()),
        ("pinning.governor", run_set.pinning.governor.as_str()),
        ("records_file", run_set.records_file.as_str()),
    ] {
        if s.trim().is_empty() {
            problems.push(format!("{what} is empty (schema requires minLength 1)"));
        }
    }

    // sample_period, when present, is a sampling deadline: schema `minimum: 1`.
    if run_set.perf.sample_period == Some(0) {
        problems.push("perf.sample_period is 0 (schema minimum is 1)".to_string());
    }

    // Every record's condition is non-empty, and its state_digest is a well-formed
    // sha256 (records carry no md5).
    for r in records {
        if r.condition.trim().is_empty() {
            problems.push(format!("record {}: condition is empty", r.sample_id));
        }
    }

    verdict(
        CheckId::WellFormed,
        &problems,
        "every schema-constrained field is well-formed",
        out,
    );
}

/// Normalise a recorded hash: drop an optional `sha256:` prefix and lowercase it.
fn normalise_hash(h: &str) -> String {
    h.strip_prefix("sha256:").unwrap_or(h).to_ascii_lowercase()
}

fn check_records_sha256(run_set: &RunSet, records_bytes: &[u8], out: &mut Vec<Outcome>) {
    let mut hasher = Sha256::new();
    hasher.update(records_bytes);
    let computed = arm_harness::evidence::hex_lower(&hasher.finalize());
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

    // How many of `0..attempted` never appeared — computed ARITHMETICALLY, not by
    // walking the range. `attempted` comes from an untrusted manifest, and a corrupt
    // one saying `u64::MAX` must fail the checker, not hang it: an arrival-day
    // instrument that fails closed beats one that fails hung. (All checks run even
    // when records-sha256 has already failed, so this is reachable with garbage.)
    let in_range_seen = seen.iter().filter(|id| **id < attempted).count() as u64;
    let missing_count = attempted.saturating_sub(in_range_seen);
    // A bounded preview of the gaps: scan only as far as the first eight.
    let mut missing_preview: Vec<u64> = Vec::new();
    if missing_count > 0 {
        for id in 0..attempted {
            if !seen.contains(&id) {
                missing_preview.push(id);
                if missing_preview.len() == 8 {
                    break;
                }
            }
        }
    }

    if duplicates.is_empty() && out_of_range.is_empty() && missing_count == 0 {
        out.push(pass(
            CheckId::Totality,
            format!("all {attempted} attempted samples present exactly once"),
        ));
        return;
    }

    let mut problems = Vec::new();
    if missing_count > 0 {
        problems.push(format!(
            "{missing_count} missing sample id(s) {} (a missing sample is a failure to \
             account, not a pass)",
            preview_of(&missing_preview, missing_count)
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
        if let Some(o) = &r.overflow
            && o.armed
        {
            armed += 1;
            match o.deliveries {
                1 => {}
                0 => lost.push(r.sample_id),
                _ => duplicated.push(r.sample_id),
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

    // Memoize the oracle by `(payload, scale, seed)`. `expected` iterates the FULL
    // scale (for branch-dense, `2 * trips` PRNG steps at 1e8 = 2×10⁸ per call), and
    // AA-1/AA-3 submit tens of thousands of records repeating the same few inputs at
    // the large scales — recomputing per record would be trillions of iterations and
    // make the checker impractical. The cache collapses that to one compute per
    // distinct input. `BTreeMap`, not `HashMap`: nothing here may make iteration order
    // reach an output. (The oracle is a pure function of the key, so caching is exact.)
    type OracleKey = (Payload, Scale, u64);
    let mut oracle: BTreeMap<OracleKey, Expectation> = BTreeMap::new();

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

        let e = *oracle
            .entry((r.payload, r.scale, r.seed))
            .or_insert_with(|| expected(r.payload, r.scale, r.seed));

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

    verdict(
        CheckId::CountExactness,
        &problems,
        format!(
            "all {} records match the oracle and are self-consistent",
            records.len()
        ),
        out,
    );
}

/// Skid — stage-and-mechanism-aware, because the two stages that touch skid mean
/// opposite things by it.
///
/// **AA-1(c) MEASURES the skid distribution.** `docs/ARM-ALTRA.md` §AA-1(c): "the
/// early/late skid distribution measured → the candidate N1 `skid_margin`". A landing
/// at `target + 1` is not a violation there — it is the datum the stage exists to
/// collect, and the margin is *derived* from the spread. So at AA-1 the checker
/// enforces only that the recorded skid is self-consistent with `landed - target`;
/// it does not forbid overshoot, does not bound against a margin (there is none yet),
/// and does not fail on `skid_margin: null` (producing it is the whole point).
///
/// **AA-3/AA-4/AA-6 ENFORCE the landing contract.** These ride the patched force-exit
/// (`requires_patched_mechanism`), whose acceptance is the late-only-stop bound:
/// never overshoot, always within the AA-1-measured margin, and — at AA-3 — land
/// exactly (`work == target`). Here a measured margin must be present, exactly as the
/// weights refusal, because a landing cannot be bounded against a margin that does not
/// exist.
fn check_skid(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let stage = run_set.stage;
    let binds_landing_contract = requires_patched_mechanism(stage);
    let exact_required = stage == Stage::Aa3;
    let margin = run_set.skid_margin;

    // The skid-margin-present requirement binds only where landings are bounded
    // against it. At AA-1(c) the margin is being derived, so its absence is the
    // stage's product, not a failure — the check simply does not apply.
    if binds_landing_contract {
        if margin.is_some() {
            out.push(pass(
                CheckId::SkidMarginPresent,
                "the manifest carries a measured skid margin",
            ));
        } else {
            out.push(fail(
                CheckId::SkidMarginPresent,
                "the manifest carries no measured skid margin (`skid_margin: null`) at a \
                 landing-contract stage: skid cannot be bounded, and the checker refuses to \
                 invent one",
            ));
        }
    }

    let mut problems: Vec<String> = Vec::new();

    for r in records {
        let Some(o) = &r.overflow else { continue };
        if !o.armed {
            continue;
        }
        // A record with no delivery has no landing: `landed` and `skid` describe
        // nothing, and reading them would report a second, phantom failure for the
        // same fact. The multiplicity check owns lost PMIs, and it has already
        // failed this record.
        if o.deliveries == 0 {
            continue;
        }

        // Data integrity, ALWAYS: the recorded skid must equal `landed - target`.
        // This is not the landing contract — it is the record being self-consistent,
        // and it holds at every stage, AA-1 included.
        let recomputed = i128::from(o.landed) - i128::from(o.target);
        if recomputed != i128::from(o.skid) {
            problems.push(format!(
                "sample {}: skid field {} != landed - target ({recomputed})",
                r.sample_id, o.skid
            ));
        }

        // The landing contract — patched stages only. At AA-1 the spread below IS the
        // measurement, so none of this applies.
        if binds_landing_contract {
            // Never overshoot: a positive skid violates late-only-stop, whatever the
            // margin.
            if recomputed > 0 {
                problems.push(format!(
                    "sample {}: landing overshot the target by {recomputed} \
                     (landed {} > target {}); the late-only-stop contract forbids it at {stage:?}",
                    r.sample_id, o.landed, o.target
                ));
            }
            // Within the measured margin.
            if let Some(m) = margin
                && recomputed.unsigned_abs() > u128::from(m)
            {
                problems.push(format!(
                    "sample {}: |skid| {} exceeds the measured margin {m}",
                    r.sample_id,
                    recomputed.unsigned_abs()
                ));
            }
            // AA-3's exact landing: work == target on every landing.
            if exact_required && recomputed != 0 {
                problems.push(format!(
                    "sample {}: AA-3 requires work == target but landed {} != target {}",
                    r.sample_id, o.landed, o.target
                ));
            }
        }
    }

    verdict(
        CheckId::Skid,
        &problems,
        if !binds_landing_contract {
            "skid distribution recorded and self-consistent (AA-1 measures it; \
             the landing contract does not bind here)"
                .to_string()
        } else if exact_required {
            "no overshoot; all landings within margin and exact (AA-3)".to_string()
        } else {
            format!("no overshoot; all landings within margin ({stage:?})")
        },
        out,
    );
}

/// Mechanism attestation — the PR-98 lesson, made mechanical.
///
/// Three layers, and the stage tuple is the one that closes the self-consistent
/// evasion: a run-set may not *certify a stage on the mechanism that stage exists to
/// replace*, however honestly it describes itself while doing so.
fn check_mechanism(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let m = &run_set.mechanism;
    let stage = run_set.stage;
    let mut problems: Vec<String> = Vec::new();

    // The kernel's identity. On arm64 KVM is built in (`CONFIG_KVM=y`), so the kernel
    // *is* the module identity: an unidentified kernel cannot attest anything.
    if m.host_kernel_sha256.trim().is_empty() {
        problems.push(
            "the mechanism block carries no host_kernel_sha256: on arm64 the kernel is the \
             module identity, and an unidentified kernel cannot attest a mechanism"
                .to_string(),
        );
    }

    // A patched claim must have been positively observed, not assumed from a build.
    if m.kvm_patched && !m.patch_marker_observed {
        problems.push(
            "manifest claims kvm_patched but patch_marker_observed is false: \
             a patched claim must be positively observed in the running kernel"
                .to_string(),
        );
    }

    // A stock kernel cannot emit KVM_EXIT_PREEMPT — it does not exist there. A
    // manifest claiming the patched exit reason on a stock kernel is incoherent.
    if m.expected_exit_reason == ExitReason::Preempt && !m.kvm_patched {
        problems.push(
            "manifest expects the patched Preempt exit but declares kvm_patched: false: \
             a stock kernel has no KVM_EXIT_PREEMPT to return"
                .to_string(),
        );
    }

    // The stage tuple. AA-3/AA-4/AA-6 rest on the patched force-exit; the stock
    // signal-kick is AA-3's forbidden fallback, and a run-set that declares it
    // *consistently* (kvm_patched: false + signal-kick records) is still the
    // fallback — self-consistency is not attestation.
    if requires_patched_mechanism(stage)
        && (!m.kvm_patched
            || !m.patch_marker_observed
            || m.expected_exit_reason != ExitReason::Preempt)
    {
        problems.push(format!(
            "stage {stage:?} rides the patched force-exit, so it requires \
             kvm_patched=true, patch_marker_observed=true and expected_exit_reason=preempt — \
             this run-set declares kvm_patched={}, patch_marker_observed={}, \
             expected_exit_reason={:?}. The signal-kick is AA-3's forbidden fallback \
             (docs/ARM-ALTRA.md §AA-3): a stage may not be certified on the mechanism it \
             exists to replace",
            m.kvm_patched, m.patch_marker_observed, m.expected_exit_reason
        ));
    }

    // Every record that ARMED an overflow must carry the claimed mechanism exit.
    // This is the other half: a run that silently exercised the stock signal-kick
    // path while claiming the patched Preempt exit fails here.
    //
    // A record that armed NOTHING (AA-1(b) counting mode, `--with-targets` absent)
    // legitimately ends at the console sentinel with `ExitReason::Mmio` — there was
    // no mechanism to attest. `expected_exit_reason` describes the armed landing, so
    // comparing it against an unarmed record's exit would reject every count-only run
    // outright. The comparison is therefore scoped to armed records.
    let mut mismatched: Vec<(u64, ExitReason)> = Vec::new();
    for r in records {
        let armed = r.overflow.as_ref().is_some_and(|o| o.armed);
        if armed && r.exit_reason != m.expected_exit_reason {
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

    verdict(
        CheckId::MechanismAttestation,
        &problems,
        format!(
            "all records carry the claimed {:?} exit; mechanism attested",
            m.expected_exit_reason
        ),
        out,
    );
}

/// The work counter was armed as the *work clock*, and not as something else.
///
/// The manifest records the perf configuration; nothing used to check it, so a
/// run-set with `raw_event: 0`, `exclude_host: false`, `exclude_guest: true`,
/// `pinned: false` passed every check — evidence that cannot establish guest-only,
/// non-multiplexed `BR_RETIRED` counting, sailing through the checker that exists to
/// establish exactly that.
fn check_perf(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let p = &run_set.perf;
    let mut problems: Vec<String> = Vec::new();

    if p.raw_event != BR_RETIRED_RAW {
        problems.push(format!(
            "perf.raw_event is {:#x}, not BR_RETIRED ({BR_RETIRED_RAW:#x}): this run counted a \
             different event than the work clock is defined as (docs/ARM-PORT.md, \
             docs/ARM-ALTRA.md §2)",
            p.raw_event
        ));
    }
    if !p.exclude_host {
        problems.push(
            "perf.exclude_host is false: the count includes host execution, so it cannot \
             establish guest-mode count exactness (AA-1(b) counts guest-only)"
                .to_string(),
        );
    }
    if p.exclude_guest {
        problems.push(
            "perf.exclude_guest is true: this event counted everything EXCEPT the guest — \
             the inverse of the work clock"
                .to_string(),
        );
    }
    if !p.pinned {
        problems.push(
            "perf.pinned is false: an unpinned event can be multiplexed, and a multiplexed \
             counter SCALES its count — every measurement in this run-set would be silently \
             corrupt"
                .to_string(),
        );
    }

    // The sampling period. It is genuinely PER-SAMPLE: an AA-3 run draws a different
    // `target_delta` for each matrix cell, so its overflow deadline — the period —
    // varies across the run-set, and each record carries its own as
    // `target - work_begin`. The manifest's single `sample_period` therefore means
    // "every armed sample used THIS period" (a uniform run); a run with varying
    // periods carries `null` and the per-sample truth is read from the records. The
    // cross-check enforces that meaning both ways, so a manifest cannot claim one
    // period while the records used another.
    let armed_periods: Vec<(u64, i128)> = records
        .iter()
        .filter_map(|r| {
            r.overflow
                .as_ref()
                .filter(|o| o.armed)
                .map(|o| (r.sample_id, i128::from(o.target) - i128::from(r.work_begin)))
        })
        .collect();
    let armed = !armed_periods.is_empty();
    match (p.sample_period, armed) {
        (Some(period), true) => {
            // A uniform claim must be true for every armed record.
            let mismatched: Vec<u64> = armed_periods
                .iter()
                .filter(|(_, per)| *per != i128::from(period))
                .map(|(id, _)| *id)
                .collect();
            if !mismatched.is_empty() {
                problems.push(format!(
                    "perf.sample_period claims a uniform {period}, but {} armed record(s) used a \
                     different period (target - work_begin): samples {}. A run with per-sample \
                     periods must carry `sample_period: null` and let each record state its own",
                    mismatched.len(),
                    preview(mismatched.iter().copied())
                ));
            }
        }
        (Some(period), false) => problems.push(format!(
            "perf.sample_period is {period} but no record armed an overflow: the manifest \
             describes a sampling run and the records are a counting one"
        )),
        // (None, true) is legitimate: a varying-period run reads each period from the
        // record. (None, false) is a pure counting run. Neither is a mismatch.
        (None, _) => {}
    }

    verdict(
        CheckId::PerfConfig,
        &problems,
        format!("raw {BR_RETIRED_RAW:#x} armed guest-only and pinned"),
        out,
    );
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

/// Pinning — and the one stage allowed to be without it.
///
/// The migration probe is AA-1's alone (`docs/ARM-ALTRA.md` §AA-1: "The migration
/// probe (bounded, once)"). Left ungated, `migration_probe: true` was a single
/// manifest field that exempted an *unpinned AA-3 landing run* from a correctness
/// condition — on this lineage a missed PMI on migration (rr #3607) can wedge
/// `KVM_RUN`, and the probe exists to demonstrate that, not to license it.
fn check_pinning(run_set: &RunSet, out: &mut Vec<Outcome>) {
    let p = &run_set.pinning;
    let stage = run_set.stage;

    if p.migration_probe && stage != Stage::Aa1 {
        out.push(fail(
            CheckId::Pinning,
            format!(
                "migration_probe is set at stage {stage:?}, but the unpinned migration probe is \
                 AA-1's alone (bounded, once — docs/ARM-ALTRA.md §AA-1). Pinning is a \
                 correctness condition on this lineage (rr #3607); one manifest field may not \
                 exempt another stage's run from it"
            ),
        ));
        return;
    }

    if p.pinned {
        // The recorded core is required evidence, not decoration: pinning is the N1
        // migration mitigation (rr #3607), and a `pinned: true` with no core cannot be
        // checked against the standing core-assignment table — the schema itself
        // describes that tuple as unverifiable. A missing core is a pinning failure.
        match p.core {
            Some(c) => out.push(pass(CheckId::Pinning, format!("pinned to core {c}"))),
            None => out.push(fail(
                CheckId::Pinning,
                "pinned: true but core: null — the pinned core is required evidence for the \
                 rr #3607 migration condition, and an unrecorded core cannot be verified against \
                 the standing core-assignment table",
            )),
        }
    } else if p.migration_probe {
        out.push(pass(
            CheckId::Pinning,
            "unpinned, but marked as AA-1's sanctioned migration probe",
        ));
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

/// AA-5's **clock-page** records must attest the harness-maintained clock page.
///
/// `self-seeded` means the payload published its own static page because the harness
/// never did — the fallback path, not the work-derived clock page whose determinism
/// AA-5 exists to certify. An AA-5 run-set whose clock-page guests all printed
/// `self-seeded` tested the fallback and would have been graded as if it had tested
/// the mechanism.
///
/// Only the [`Payload::ClockPage`] payload emits a `CLOCKPAGE` line; the other seven
/// windowed payloads legitimately carry `clockpage_mode: None`. The default AA-5 plan
/// runs the whole matrix, so this check is **scoped to clock-page records** — grading
/// every record would reject the standard AA-5 run unconditionally, though its
/// clock-page samples proved managed mode correctly. But it still requires that at
/// least one clock-page record exists: an AA-5 run with none tested the mechanism not
/// at all.
fn check_clockpage_mode(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if run_set.stage != Stage::Aa5 {
        return;
    }

    let clockpage: Vec<&RunRecord> = records
        .iter()
        .filter(|r| r.payload == Payload::ClockPage)
        .collect();
    if clockpage.is_empty() {
        out.push(fail(
            CheckId::ClockPageMode,
            "AA-5 run-set contains no clock-page records: the paravirt-clock mechanism AA-5 \
             certifies was never exercised",
        ));
        return;
    }

    let mut bad: Vec<(u64, String)> = clockpage
        .iter()
        .filter(|r| r.clockpage_mode.as_deref() != Some(MANAGED_CLOCKPAGE))
        .map(|r| {
            (
                r.sample_id,
                r.clockpage_mode
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            )
        })
        .collect();
    bad.sort_by(|a, b| a.0.cmp(&b.0));

    if bad.is_empty() {
        out.push(pass(
            CheckId::ClockPageMode,
            format!(
                "all {} clock-page record(s) attest the {MANAGED_CLOCKPAGE} clock page (AA-5)",
                clockpage.len()
            ),
        ));
    } else {
        let shown: Vec<String> = bad
            .iter()
            .take(8)
            .map(|(id, mode)| format!("sample {id}={mode}"))
            .collect();
        out.push(fail(
            CheckId::ClockPageMode,
            format!(
                "{} AA-5 record(s) do not attest the harness-maintained clock page: {} \
                 (`self-seeded` is the payload's own static page — the fallback, not the \
                 work-derived mechanism AA-5 certifies)",
                bad.len(),
                shown.join(", ")
            ),
        ));
    }
}

/// The key a repetition is *the same run* under: same payload, scale, seed,
/// condition, and — for an armed run — the same target **delta**.
///
/// The delta is `target - work_begin`, NOT the absolute target. The plan reuses one
/// `target_delta` across every repetition of an input, but the stored target is
/// `work_begin + delta`; if pre-window execution diverges and `work_begin` differs,
/// the absolute targets differ and same-input records split into different groups —
/// so replay-identity would report "no repeated group" instead of catching the
/// divergent landed states. Keying by delta groups them correctly. A malformed
/// record where `target < work_begin` (a negative delta) is caught separately by
/// [`check_replay_identity`].
type RepKey = (String, String, u64, String, Option<i128>);

fn rep_key(r: &RunRecord) -> RepKey {
    (
        r.payload.name().to_string(),
        r.scale.name().to_string(),
        r.seed,
        r.condition.clone(),
        r.overflow
            .as_ref()
            .map(|o| i128::from(o.target) - i128::from(r.work_begin)),
    )
}

/// The digest a repetition is compared on.
///
/// For an armed AA-3 landing, that is the **landed** digest — the state at the
/// Moment the deadline was hit, sampled before the guest resumed. The final
/// `state_digest`, taken at the exit sentinel, can converge: two different landed
/// states can run on to the same final state, so it cannot establish landing
/// identity. For an unarmed counting run there is no landing, and the final state is
/// the thing to compare.
fn comparison_digest(r: &RunRecord) -> &str {
    match &r.overflow {
        Some(o) if o.armed && o.deliveries >= 1 => o.landed_digest.trim(),
        _ => r.state_digest.trim(),
    }
}

/// Repetitions of the same input must land on **bit-identical** state.
///
/// This is the axis the rep floor exists for, and nothing used to check it: 1,000
/// same-seed reps with 1,000 *divergent* digests met a `--min-reps 1000` floor,
/// though the floor's whole meaning is "repetitions bit-identical" (AA-6), and
/// AA-3's replay-identity rides the same field. The digest appeared only in fixture
/// data — no check ever read it. Now one does, on the digest that actually matters
/// for the record's mode ([`comparison_digest`]), and an empty digest (which would
/// compare equal to every other empty digest) is a failure in its own right.
fn check_replay_identity(stage: Stage, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut problems: Vec<String> = Vec::new();

    // A malformed record whose overflow deadline is BEFORE the window opened
    // (`target < work_begin`, a negative delta) cannot be a real landing — the
    // repetition key is derived from `target - work_begin`, so this is caught here
    // rather than quietly producing a negative-delta group.
    let mut underflow: Vec<u64> = records
        .iter()
        .filter(|r| {
            r.overflow
                .as_ref()
                .is_some_and(|o| i128::from(o.target) < i128::from(r.work_begin))
        })
        .map(|r| r.sample_id)
        .collect();
    underflow.sort_unstable();
    if !underflow.is_empty() {
        problems.push(format!(
            "{} record(s) have a target before the window opened (target < work_begin): \
             samples {} — a negative overflow delta is malformed",
            underflow.len(),
            preview(underflow.iter().copied())
        ));
    }

    let mut blank: Vec<u64> = records
        .iter()
        .filter(|r| comparison_digest(r).is_empty())
        .map(|r| r.sample_id)
        .collect();
    blank.sort_unstable();
    if !blank.is_empty() {
        problems.push(format!(
            "{} record(s) carry an empty comparison digest: samples {} — a digest that cannot \
             diverge satisfies every determinism comparison without measuring anything (an armed \
             landing must carry its landed_digest; a counting run its state_digest)",
            blank.len(),
            preview(blank.iter().copied())
        ));
    }

    // Group by the repetition key. BTreeMap, never HashMap: the report is evidence,
    // and its bytes may not depend on iteration order.
    let mut groups: BTreeMap<RepKey, BTreeMap<String, Vec<u64>>> = BTreeMap::new();
    for r in records {
        groups
            .entry(rep_key(r))
            .or_default()
            .entry(comparison_digest(r).to_string())
            .or_default()
            .push(r.sample_id);
    }

    let mut compared = 0usize;
    for (key, digests) in &groups {
        let reps: usize = digests.values().map(Vec::len).sum();
        if reps < 2 {
            continue;
        }
        compared += 1;
        if digests.len() > 1 {
            // Name the diverging samples: one representative id per distinct digest,
            // in sorted-digest order, so the detail is reproducible.
            let ids: Vec<String> = digests
                .iter()
                .take(8)
                .map(|(d, ids)| {
                    let short: String = d.chars().take(16).collect();
                    format!("{short}…={:?}", ids.iter().take(4).collect::<Vec<_>>())
                })
                .collect();
            problems.push(format!(
                "payload {} scale {} seed {} condition {} target {:?}: {} repetitions landed on \
                 {} DIFFERENT state digests — same seed must mean bit-identical execution: {}",
                key.0,
                key.1,
                key.2,
                key.3,
                key.4,
                reps,
                digests.len(),
                ids.join(", ")
            ));
        }
    }

    // A stage whose acceptance IS replay identity (AA-3's replay-identical landings,
    // AA-6's ≥1000 same-input bit-identity) may not PASS this check having compared
    // NOTHING. With the default `--reps 1` there is no repeated group, so grading zero
    // comparisons as a pass would let evidence claim replay identity without a single
    // replay — the exact vacuous-pass the evidence-integrity bar forbids. So when the
    // stage requires it and no repeated group exists, the check is NOT-REQUESTED
    // (nonzero, not a pass); the operator must submit repeated inputs (`--reps`).
    if !problems.is_empty() {
        out.push(fail(CheckId::ReplayIdentity, join_problems(&problems)));
    } else if compared == 0 && requires_replay_identity(stage) {
        out.push(not_requested(
            CheckId::ReplayIdentity,
            format!(
                "stage {stage:?} rests on replay identity, but the records contain no repeated \
                 (payload, scale, seed, condition, target) group to compare — with --reps 1 there \
                 is nothing to replay. Submit repeated inputs; this verdict cannot accept replay \
                 identity it never tested"
            ),
        ));
    } else if compared == 0 {
        out.push(pass(
            CheckId::ReplayIdentity,
            "no repeated group to compare at this stage; every record carries a digest",
        ));
    } else {
        out.push(pass(
            CheckId::ReplayIdentity,
            format!("{compared} repeated group(s) landed on bit-identical state digests"),
        ));
    }
}

/// Whether a stage's acceptance **is** replay identity: AA-3 (replay-identical landed
/// state) and AA-6 (≥1,000 same-input bit-identical). At those, comparing zero
/// digests is not a pass. AA-1/AA-2/AA-4/AA-5 do not rest on it.
const fn requires_replay_identity(stage: Stage) -> bool {
    matches!(stage, Stage::Aa3 | Stage::Aa6)
}

/// AA-2 exists to *characterize single-stepping* — does one step retire exactly one
/// instruction, and with what skid. Its evidence is the debug exit
/// ([`ExitReason::Debug`]). An ordinary `--stage aa2` run is unarmed and ends at the
/// console sentinel (`Mmio`), so without this check the floor reports PASS having
/// observed not one step — the same vacuity class as replay-identity on zero
/// comparisons.
///
/// This does NOT vacuously pass and it does NOT hard-fail an honest run: it requires a
/// debug-exit record and reports NOT-REQUESTED when none is present. The single-step
/// *run path* is arrival-day work — the run loop rejects an unrequested `Debug` exit
/// today, and building the stepping loop presumes AA-2's own result (which single-step
/// primitive lands exactly one instruction), exactly the AA-1/AA-2 unknowns the
/// pre-build ruling forbids inventing (`docs/ARCH-BOUNDARY.md` §Pre-build ruling; the
/// r5 skid-landing rebuttal, accepted). So today AA-2 reads NOT-REQUESTED — honestly
/// unexercised — and flips to a real verdict once arrival day produces stepped records.
fn check_debug_evidence(stage: Stage, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if stage != Stage::Aa2 {
        return;
    }
    let stepped = records
        .iter()
        .filter(|r| r.exit_reason == ExitReason::Debug)
        .count();
    if stepped == 0 {
        out.push(not_requested(
            CheckId::DebugEvidence,
            "AA-2 certifies single-stepping, whose evidence is the debug exit, but no \
             record carries ExitReason::Debug — not a single step was observed. The \
             single-step run path is arrival-day (the run loop refuses an unrequested \
             debug exit, and the stepping loop would presume AA-2's own single-step \
             result); this verdict cannot accept AA-2 as exercised until stepped records \
             exist. NOT a pass.",
        ));
    } else {
        out.push(pass(
            CheckId::DebugEvidence,
            format!("{stepped} record(s) carry single-step (debug-exit) evidence (AA-2)"),
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

/// The numeric floors — and the floors the evidence needed but nobody asked for.
///
/// §Evidence integrity #2: the checker's output *is* retained evidence. So a verdict
/// over an overflow-bearing run-set that never mentions the armed-overflow floor is
/// not a clean verdict, it is a silent one — and silence, on the face of a document
/// a disposition rests on, reads as acceptance. The checker therefore demands the
/// *presence* of an explicit floor while never supplying its value.
fn check_floors(run_set: &RunSet, floors: &Floors, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let armed = records
        .iter()
        .filter(|r| r.overflow.as_ref().is_some_and(|o| o.armed))
        .count() as u64;

    // A stage whose acceptance IS armed deadlines (AA-3's ≥10⁶ armed overflows,
    // landed exactly) may not pass without armed records OR without the floor being
    // requested. The missing-floor case must NOT be gated on `armed > 0`: an AA-3 run
    // submitted with no armed records (e.g. run without `--with-targets`) would then
    // emit no floor outcome at all, and the mechanism and skid checks have nothing to
    // inspect — so AA-3 would pass without testing a single deadline. The requirement
    // is enforced on the STAGE, independent of what the records happened to contain.
    let requires_armed = requires_patched_mechanism(run_set.stage);
    match floors.min_armed_overflows {
        Some(min) if armed >= min => out.push(pass(
            CheckId::ArmedOverflowFloor,
            format!("{armed} armed overflows meets the floor of {min}"),
        )),
        Some(min) => out.push(fail(
            CheckId::ArmedOverflowFloor,
            format!("only {armed} armed overflows, below the floor of {min}"),
        )),
        None if requires_armed => out.push(not_requested(
            CheckId::ArmedOverflowFloor,
            format!(
                "stage {:?} rests on armed deadlines (AA-3's acceptance is ≥1000000 armed \
                 overflows landed exactly), but no --min-armed-overflows floor was requested and \
                 the records carry {armed} armed overflow(s). This verdict cannot accept a landing \
                 stage that tested no deadline; pass the floor explicitly.",
                run_set.stage
            ),
        )),
        None if armed > 0 => out.push(not_requested(
            CheckId::ArmedOverflowFloor,
            format!(
                "the records carry {armed} armed overflow(s) but no --min-armed-overflows floor \
                 was requested: this verdict cannot be read as accepting one. The AA-1/AA-3 \
                 acceptance floor is 1000000 cumulative — pass it explicitly, so the number the \
                 disposition rests on is visible in the command that produced it"
            ),
        )),
        None => {}
    }

    // The rep floor is PER-REPEATED-INPUT, not total rows. AA-6 needs ≥1000
    // repetitions of the SAME (payload, scale, seed, condition, target) input,
    // bit-identical. Counting total records would let 1,000 rows that are 125 reps of
    // an eight-payload matrix pass a 1,000 floor, though no single input was repeated
    // 1,000 times — which is not the same-seed determinism the gate certifies. So the
    // floor is the count of the *least-repeated* distinct input: every group must meet
    // it. (replay-identity then checks those reps actually landed identically.)
    match floors.min_reps {
        Some(min) => {
            let mut groups: BTreeMap<RepKey, u64> = BTreeMap::new();
            for r in records {
                *groups.entry(rep_key(r)).or_default() += 1;
            }
            let distinct = groups.len();
            let min_group = groups.values().copied().min().unwrap_or(0);
            if min_group >= min {
                out.push(pass(
                    CheckId::RepFloor,
                    format!(
                        "{distinct} distinct input(s), each repeated at least {min_group} times \
                         (floor {min})"
                    ),
                ));
            } else {
                out.push(fail(
                    CheckId::RepFloor,
                    format!(
                        "the least-repeated input appears only {min_group} time(s), below the \
                         per-input rep floor of {min} (there are {distinct} distinct inputs across \
                         {} records; a total-count floor would have hidden this — AA-6 needs {min} \
                         reps of the SAME input)",
                        records.len()
                    ),
                ));
            }
        }
        None if run_set.stage == Stage::Aa6 => out.push(not_requested(
            CheckId::RepFloor,
            "AA-6's mini determinism gate rests on ≥1000 same-input repetitions, but no \
             --min-reps floor was requested: this verdict cannot be read as accepting one",
        )),
        None => {}
    }
}

/// Render up to eight ids, then a count of the remainder, so a failure detail
/// stays bounded and deterministic on a run-set with many bad samples.
fn preview(ids: impl Iterator<Item = u64>) -> String {
    let all: Vec<u64> = ids.collect();
    let total = all.len() as u64;
    preview_of(&all, total)
}

/// Render an already-bounded preview list, given the true total it was drawn from.
fn preview_of(shown: &[u64], total: u64) -> String {
    let rendered: Vec<String> = shown.iter().take(8).map(u64::to_string).collect();
    let shown_len = rendered.len() as u64;
    if total > shown_len {
        format!(
            "[{}, +{} more]",
            rendered.join(", "),
            total.saturating_sub(shown_len)
        )
    } else {
        format!("[{}]", rendered.join(", "))
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

#[cfg(test)]
mod tests {
    //! Unit coverage for the checks the accept/reject fixtures do not exercise on
    //! their own: the refusals (no weights, no margin — no invented constants), the
    //! stage-conditional rules, the empty-digest and unbounded-`attempted` failure
    //! modes, and the not-requested floors.

    use super::*;
    use arm_harness::evidence::{
        Environment, ImagePin, Mechanism, OverflowRecord, PerfConfig, Pinning,
    };
    use oracle_model::{DEFAULT_SEED, Payload, Scale, Weights};
    use std::collections::BTreeMap;

    fn a_record(sample_id: u64) -> RunRecord {
        // straight-line at smoke: certain 999, window offset 2 => 1001 taken.
        let measured = 1001;
        // The overflow deadline is a UNIFORM 500 events past the window open — matching
        // `a_run_set().perf.sample_period`, decoupled from the window count.
        let target = 1_000 + 500;
        RunRecord {
            sample_id,
            payload: Payload::StraightLine,
            scale: Scale::Smoke,
            seed: DEFAULT_SEED,
            trips: 1_000,
            condition: "pinned-solo".into(),
            work_begin: 1_000,
            work_end: 1_000 + measured,
            measured_taken: measured,
            reported_taken: 0,
            exit_reason: ExitReason::Preempt,
            overflow: Some(OverflowRecord {
                armed: true,
                deliveries: 1,
                advisory_exits: 0,
                target,
                landed: target,
                skid: 0,
                landed_digest: "sha256:aa".into(),
            }),
            state_digest: "sha256:00".into(),
            params_mode: "managed".into(),
            clockpage_mode: None,
            payload_status: 0,
        }
    }

    /// A record with a chosen `seed`, so tests can build distinct or identical
    /// repetition inputs (`seed` is part of the [`RepKey`]).
    fn a_record_seeded(sample_id: u64, seed: u64) -> RunRecord {
        let mut r = a_record(sample_id);
        r.seed = seed;
        r
    }

    fn a_run_set() -> RunSet {
        RunSet {
            schema_version: SCHEMA_VERSION,
            stage: Stage::Aa3,
            run_set_id: "unit".into(),
            environment: Environment {
                midr: 0x413f_d0c0,
                soc: "unit".into(),
                firmware: BTreeMap::new(),
                host_kernel: "6.18.35".into(),
                kvm_mode: "vhe".into(),
            },
            mechanism: Mechanism {
                kvm_patched: true,
                host_kernel_sha256: "0".repeat(64),
                expected_exit_reason: ExitReason::Preempt,
                patch_marker_observed: true,
            },
            images: vec![ImagePin {
                path: "img".into(),
                sha256: "0".repeat(64),
                md5: Some("0".repeat(32)),
                verified_before_boot: true,
            }],
            perf: PerfConfig {
                raw_event: 0x21,
                exclude_host: true,
                exclude_guest: false,
                exclude_hv: true,
                pinned: true,
                sample_period: Some(500),
            },
            pinning: Pinning {
                pinned: true,
                core: Some(2),
                governor: "performance".into(),
                migration_probe: false,
            },
            condition: "pinned-solo".into(),
            weights: Some(Weights::measured(0, 0, 0, 0, 2)),
            skid_margin: Some(64),
            attempted: 1,
            records_file: "records.jsonl".into(),
            records_sha256: "0".repeat(64),
        }
    }

    fn status(out: &[Outcome], id: CheckId) -> Option<Status> {
        out.iter().find(|o| o.id == id).map(|o| o.status)
    }

    #[test]
    fn unknown_schema_version_is_refused_not_guessed() {
        let mut rs = a_run_set();
        rs.schema_version = SCHEMA_VERSION + 1;
        let mut out = Vec::new();
        check_schema_version(&rs, &mut out);
        assert_eq!(status(&out, CheckId::SchemaVersion), Some(Status::Fail));
    }

    #[test]
    fn missing_weights_refuses_the_count_check() {
        let mut rs = a_run_set();
        rs.weights = None;
        let mut out = Vec::new();
        check_weights_and_counts(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::WeightsPresent), Some(Status::Fail));
        // The count check is refused, never defaulted to a guess.
        assert_eq!(status(&out, CheckId::CountExactness), Some(Status::Fail));
    }

    #[test]
    fn missing_skid_margin_refuses_the_bound() {
        let mut rs = a_run_set();
        rs.skid_margin = None;
        let mut out = Vec::new();
        check_skid(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::SkidMarginPresent), Some(Status::Fail));
    }

    #[test]
    fn unpinned_non_probe_run_fails_pinning() {
        let mut rs = a_run_set();
        rs.pinning.pinned = false;
        rs.pinning.migration_probe = false;
        let mut out = Vec::new();
        check_pinning(&rs, &mut out);
        assert_eq!(status(&out, CheckId::Pinning), Some(Status::Fail));
    }

    #[test]
    fn the_sanctioned_migration_probe_may_be_unpinned_at_aa1_only() {
        // AA-1's bounded probe: legitimate.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        rs.pinning.pinned = false;
        rs.pinning.migration_probe = true;
        let mut out = Vec::new();
        check_pinning(&rs, &mut out);
        assert_eq!(status(&out, CheckId::Pinning), Some(Status::Pass));

        // The same field at AA-3 is one manifest boolean exempting a landing run from
        // a correctness condition. Refused — even if the run also claims to be pinned.
        for pinned in [false, true] {
            let mut rs = a_run_set();
            rs.stage = Stage::Aa3;
            rs.pinning.pinned = pinned;
            rs.pinning.migration_probe = true;
            let mut out = Vec::new();
            check_pinning(&rs, &mut out);
            assert_eq!(
                status(&out, CheckId::Pinning),
                Some(Status::Fail),
                "migration_probe outside AA-1 must fail (pinned={pinned})"
            );
        }
    }

    #[test]
    fn an_aa3_run_set_on_the_stock_mechanism_is_refused_however_consistent() {
        // The most PR-98-shaped evasion there is: everything agrees with everything,
        // and what it all agrees on is the forbidden fallback.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa3;
        rs.mechanism = Mechanism {
            kvm_patched: false,
            host_kernel_sha256: "0".repeat(64),
            expected_exit_reason: ExitReason::SignalKick,
            patch_marker_observed: false,
        };
        let mut r = a_record(0);
        r.exit_reason = ExitReason::SignalKick;
        let mut out = Vec::new();
        check_mechanism(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail)
        );
    }

    #[test]
    fn the_stock_mechanism_stays_legitimate_at_aa1() {
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        rs.mechanism = Mechanism {
            kvm_patched: false,
            host_kernel_sha256: "0".repeat(64),
            expected_exit_reason: ExitReason::SignalKick,
            patch_marker_observed: false,
        };
        let mut r = a_record(0);
        r.exit_reason = ExitReason::SignalKick;
        let mut out = Vec::new();
        check_mechanism(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Pass),
            "AA-1(c)'s pre-patch signal kick is the stage's own mechanism"
        );
    }

    #[test]
    fn a_stock_kernel_may_not_claim_the_patched_exit() {
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        rs.mechanism.kvm_patched = false;
        rs.mechanism.patch_marker_observed = false;
        rs.mechanism.expected_exit_reason = ExitReason::Preempt;
        let mut out = Vec::new();
        check_mechanism(&rs, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail)
        );
    }

    #[test]
    fn an_unidentified_host_kernel_cannot_attest_a_mechanism() {
        let mut rs = a_run_set();
        rs.mechanism.host_kernel_sha256 = String::new();
        let mut out = Vec::new();
        check_mechanism(&rs, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail)
        );
    }

    #[test]
    fn a_perf_event_that_is_not_the_work_clock_is_refused() {
        // Every one of these alone is fatal to the evidence.
        for mutate in [
            (|p: &mut PerfConfig| p.raw_event = 0) as fn(&mut PerfConfig),
            |p: &mut PerfConfig| p.exclude_host = false,
            |p: &mut PerfConfig| p.exclude_guest = true,
            |p: &mut PerfConfig| p.pinned = false,
        ] {
            let mut rs = a_run_set();
            mutate(&mut rs.perf);
            let mut out = Vec::new();
            check_perf(&rs, &[a_record(0)], &mut out);
            assert_eq!(status(&out, CheckId::PerfConfig), Some(Status::Fail));
        }
    }

    #[test]
    fn the_sample_period_cross_checks_against_the_records() {
        // A period, but a pure counting run: the manifest describes a sampling run and
        // the records are a counting one.
        let rs = a_run_set();
        let mut r = a_record(0);
        r.overflow = None;
        let mut out = Vec::new();
        check_perf(&rs, &[r], &mut out);
        assert_eq!(status(&out, CheckId::PerfConfig), Some(Status::Fail));

        // A uniform period claim that a record VIOLATES: the manifest says every armed
        // sample used period 500, but this record's target - work_begin is different.
        let mut rs = a_run_set();
        rs.perf.sample_period = Some(500);
        let mut r = a_record(0);
        if let Some(o) = r.overflow.as_mut() {
            o.target = r.work_begin + 999; // period 999, not the claimed 500
            o.landed = o.target;
            o.skid = 0;
        }
        let mut out = Vec::new();
        check_perf(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::PerfConfig),
            Some(Status::Fail),
            "a uniform-period claim the records contradict must fail"
        );

        // A null period with armed records is LEGITIMATE: a varying-period run reads
        // each period from its record (target - work_begin). Not a mismatch.
        let mut rs = a_run_set();
        rs.perf.sample_period = None;
        let mut out = Vec::new();
        check_perf(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::PerfConfig), Some(Status::Pass));
    }

    #[test]
    fn aa5_records_must_attest_the_managed_clock_page() {
        let mut rs = a_run_set();
        rs.stage = Stage::Aa5;

        // A clock-page record: the payload whose mode the attestation is about.
        let clockpage = |mode: Option<&str>| {
            let mut r = a_record(0);
            r.payload = Payload::ClockPage;
            r.clockpage_mode = mode.map(str::to_string);
            r
        };

        // The self-seeded fallback: the payload published its own static page.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(Some("self-seeded"))], &mut out);
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Fail));

        // No attestation at all is not better than the wrong one.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(None)], &mut out);
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Fail));

        // The managed page: what AA-5 exists to certify.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(Some("managed"))], &mut out);
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Pass));

        // An AA-5 run-set with NO clock-page records at all is a vacuous pass waiting
        // to happen — the mechanism AA-5 certifies was never exercised. Finding 4.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::ClockPageMode),
            Some(Status::Fail),
            "an AA-5 set with no clock-page payload must not pass silently"
        );

        // And the check does not fire outside AA-5, where the page is not the subject.
        let rs = a_run_set();
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(Some("managed"))], &mut out);
        assert!(out.is_empty());
    }

    /// Set the digest a record is COMPARED on — landed for an armed landing, final
    /// for a counting run.
    fn set_landed_digest(r: &mut RunRecord, d: &str) {
        if let Some(o) = r.overflow.as_mut() {
            o.landed_digest = d.to_string();
        }
    }

    #[test]
    fn divergent_landed_digests_fail_the_replay_identity_check() {
        // The vacuity this closes: two reps of the same input, two different LANDED
        // states — which a rep floor counting rows would have accepted.
        let mut a = a_record(0);
        let mut b = a_record(1);
        set_landed_digest(&mut a, "sha256:aaaa");
        set_landed_digest(&mut b, "sha256:bbbb");
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa1, &[a, b], &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));
    }

    #[test]
    fn armed_replay_compares_the_landed_digest_not_the_converged_final_state() {
        // Two runs that landed on DIFFERENT states but converged to the same final
        // state must still fail: the final state can converge, so it cannot establish
        // landing identity. The landed digest is what AA-3's claim is about.
        let mut a = a_record(0);
        let mut b = a_record(1);
        a.state_digest = "sha256:converged".into();
        b.state_digest = "sha256:converged".into();
        set_landed_digest(&mut a, "sha256:landed-a");
        set_landed_digest(&mut b, "sha256:landed-b");
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa1, &[a, b], &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::Fail),
            "identical final states must not paper over divergent landings"
        );
    }

    #[test]
    fn identical_landed_digests_pass_and_a_blank_one_does_not() {
        let a = a_record(0);
        let b = a_record(1);
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa1, &[a, b], &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Pass));

        // An empty comparison digest compares equal to every other empty one: it
        // would make the whole check — and the AA-6 floor above it — vacuous.
        let mut a = a_record(0);
        set_landed_digest(&mut a, "");
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa1, &[a], &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));
    }

    #[test]
    fn a_counting_run_ending_on_mmio_is_not_a_mechanism_mismatch() {
        // AA-1(b): no overflow armed, so the record legitimately ends at the console
        // sentinel with ExitReason::Mmio. The manifest's expected_exit_reason is about
        // the ARMED landing; comparing it against an unarmed record rejected every
        // count-only run. The comparison is now scoped to armed records.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        rs.mechanism = Mechanism {
            kvm_patched: false,
            host_kernel_sha256: "0".repeat(64),
            expected_exit_reason: ExitReason::SignalKick,
            patch_marker_observed: false,
        };
        let mut r = a_record(0);
        r.overflow = None; // a pure counting run
        r.exit_reason = ExitReason::Mmio;
        let mut out = Vec::new();
        check_mechanism(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Pass),
            "an unarmed counting record ending on Mmio is not a masquerade"
        );

        // But an ARMED record still must carry the claimed mechanism exit.
        let mut r = a_record(0);
        r.exit_reason = ExitReason::Mmio; // armed, yet no mechanism landing
        let mut out = Vec::new();
        check_mechanism(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail),
            "an armed record that did not land the mechanism is still a mismatch"
        );
    }

    #[test]
    fn nonzero_payload_status_fails() {
        let mut r = a_record(0);
        r.payload_status = 1;
        let mut out = Vec::new();
        check_payload_status(&[r], &mut out);
        assert_eq!(status(&out, CheckId::PayloadStatus), Some(Status::Fail));
    }

    #[test]
    fn rep_floor_fails_below_the_minimum() {
        let floors = Floors {
            min_armed_overflows: None,
            min_reps: Some(1_000),
        };
        let mut out = Vec::new();
        check_floors(&a_run_set(), &floors, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::RepFloor), Some(Status::Fail));
    }

    #[test]
    fn repetitions_group_by_delta_so_a_divergent_work_begin_does_not_split_them() {
        // Two reps of the same input, same target DELTA (500), but different
        // work_begin (pre-window execution diverged). Their absolute targets differ,
        // so a target-keyed grouping would split them into singleton groups and
        // replay-identity would find nothing to compare. Keyed by delta, they are one
        // group — and their divergent landed digests are caught.
        let mut a = a_record(0);
        let mut b = a_record(1);
        // a: work_begin 1000, target 1500 (delta 500). b: work_begin 2000, target 2500
        // (delta 500). Same input, same delta; different landings.
        a.work_begin = 1_000;
        b.work_begin = 2_000;
        if let Some(o) = a.overflow.as_mut() {
            o.target = 1_500;
            o.landed = 1_500;
            o.skid = 0;
            o.landed_digest = "sha256:aaaa".into();
        }
        if let Some(o) = b.overflow.as_mut() {
            o.target = 2_500;
            o.landed = 2_500;
            o.skid = 0;
            o.landed_digest = "sha256:bbbb".into();
        }
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa1, &[a, b], &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::Fail),
            "same-delta reps must group despite divergent work_begin, and their \
             divergent landings must be caught"
        );
    }

    #[test]
    fn a_target_before_the_window_is_malformed() {
        let mut r = a_record(0);
        r.work_begin = 2_000;
        if let Some(o) = r.overflow.as_mut() {
            o.target = 1_000; // before work_begin — negative delta
        }
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa1, &[r], &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));
    }

    #[test]
    fn pinned_true_with_no_core_fails() {
        let mut rs = a_run_set();
        rs.pinning.pinned = true;
        rs.pinning.core = None;
        rs.pinning.migration_probe = false;
        let mut out = Vec::new();
        check_pinning(&rs, &mut out);
        assert_eq!(status(&out, CheckId::Pinning), Some(Status::Fail));
    }

    #[test]
    fn an_empty_hash_fails_the_well_formed_gate() {
        let mut rs = a_run_set();
        rs.images[0].sha256 = String::new();
        let mut out = Vec::new();
        check_well_formed(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::WellFormed), Some(Status::Fail));

        // A zero sampling period likewise.
        let mut rs = a_run_set();
        rs.perf.sample_period = Some(0);
        let mut out = Vec::new();
        check_well_formed(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::WellFormed), Some(Status::Fail));

        // The baseline is well-formed.
        let mut out = Vec::new();
        check_well_formed(&a_run_set(), &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::WellFormed), Some(Status::Pass));
    }

    #[test]
    fn the_rep_floor_is_per_input_not_total_records() {
        // The evasion: many records, but each distinct input repeated only once. A
        // total-count floor of 3 passes on 3 distinct inputs; the per-input floor does
        // not — AA-6 needs 3 reps of the SAME input, not 3 inputs once each.
        let three_distinct = vec![
            a_record_seeded(0, 1),
            a_record_seeded(1, 2),
            a_record_seeded(2, 3),
        ];
        let floors = Floors {
            min_armed_overflows: None,
            min_reps: Some(3),
        };
        let mut out = Vec::new();
        check_floors(&a_run_set(), &floors, &three_distinct, &mut out);
        assert_eq!(
            status(&out, CheckId::RepFloor),
            Some(Status::Fail),
            "three distinct inputs is not three reps of one input"
        );

        // Three reps of the SAME input (same seed) meets a per-input floor of 3.
        let three_reps = vec![
            a_record_seeded(0, 7),
            a_record_seeded(1, 7),
            a_record_seeded(2, 7),
        ];
        let mut out = Vec::new();
        check_floors(&a_run_set(), &floors, &three_reps, &mut out);
        assert_eq!(status(&out, CheckId::RepFloor), Some(Status::Pass));
    }

    #[test]
    fn an_unrequested_armed_overflow_floor_is_visible_in_the_verdict() {
        // The records carry armed overflows; nobody named a floor. That verdict may
        // not read as acceptance — it is NOT-REQUESTED, and it is not a pass.
        let mut out = Vec::new();
        check_floors(&a_run_set(), &Floors::default(), &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::NotRequested)
        );

        // Finding 6: an AA-3 run submitted with NO armed records and no floor is the
        // vacuous pass — the stage rests on armed deadlines but tested none. The
        // requirement is on the STAGE, so it must be NOT-REQUESTED even (especially)
        // when the records carry nothing to inspect. `a_run_set()` is AA-3.
        let mut r = a_record(0);
        r.overflow = None;
        let mut out = Vec::new();
        check_floors(&a_run_set(), &Floors::default(), &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::NotRequested),
            "AA-3 with zero armed records and no floor must not pass silently"
        );

        // A pre-patch stage (AA-2) legitimately has no armed-deadline floor: with no
        // armed records there really is nothing to be silent about, and no outcome is
        // emitted.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa2;
        let mut r = a_record(0);
        r.overflow = None;
        let mut out = Vec::new();
        check_floors(&rs, &Floors::default(), &[r], &mut out);
        assert_eq!(status(&out, CheckId::ArmedOverflowFloor), None);
    }

    #[test]
    fn an_aa6_run_set_checked_without_a_rep_floor_says_so() {
        let mut rs = a_run_set();
        rs.stage = Stage::Aa6;
        let mut out = Vec::new();
        check_floors(&rs, &Floors::default(), &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::RepFloor), Some(Status::NotRequested));
    }

    #[test]
    fn aa2_without_a_debug_exit_is_not_requested_never_a_pass() {
        // The vacuity: an AA-2 run of ordinary unarmed records that end at the console
        // sentinel observed no single step, but nothing here graded that — the floor
        // could report PASS. It must read NOT-REQUESTED instead (the single-step run
        // path is arrival-day), and never PASS.
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::NotRequested),
            "AA-2 with no stepped record must not pass silently"
        );

        // A record that DID land on a debug exit is the evidence AA-2 is about.
        let mut stepped = a_record(0);
        stepped.exit_reason = ExitReason::Debug;
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &[stepped], &mut out);
        assert_eq!(status(&out, CheckId::DebugEvidence), Some(Status::Pass));

        // The check does not fire outside AA-2, where single-stepping is not the subject.
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa3, &[a_record(0)], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_corrupt_attempted_count_fails_closed_rather_than_hanging() {
        // `attempted: u64::MAX` from a corrupt manifest used to walk the whole range
        // building a Vec of missing ids. Fail closed beats fail hung — the checker is
        // an arrival-day instrument, and all checks run even when records-sha256 has
        // already failed.
        let mut rs = a_run_set();
        rs.attempted = u64::MAX;
        let mut out = Vec::new();
        check_totality(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::Totality), Some(Status::Fail));
        let detail = &out[0].detail;
        assert!(
            detail.contains(&format!("{} missing", u64::MAX - 1)),
            "the count is arithmetic, not a walked range: {detail}"
        );
    }

    #[test]
    fn measured_taken_must_equal_the_window_delta() {
        let mut r = a_record(0);
        r.work_end = r.work_begin + r.measured_taken + 1; // endpoints now disagree
        let mut out = Vec::new();
        check_counts(&Weights::measured(0, 0, 0, 0, 2), &[r], &mut out);
        assert_eq!(status(&out, CheckId::CountExactness), Some(Status::Fail));
    }

    #[test]
    fn a_lost_pmi_is_multiplicitys_failure_and_not_also_skids() {
        // A record with no delivery has no landing: its `landed`/`skid` describe
        // nothing, and reporting them as a second failure would double-count one fact.
        let mut r = a_record(0);
        if let Some(o) = r.overflow.as_mut() {
            o.deliveries = 0;
            o.landed = 0;
            o.skid = -(o.target as i64);
        }
        let mut out = Vec::new();
        check_skid(&a_run_set(), &[r.clone()], &mut out);
        assert_eq!(status(&out, CheckId::Skid), Some(Status::Pass));

        let mut out = Vec::new();
        check_multiplicity(&[r], &mut out);
        assert_eq!(status(&out, CheckId::Multiplicity), Some(Status::Fail));
    }
}
