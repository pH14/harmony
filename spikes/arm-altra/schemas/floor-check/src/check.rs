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
use std::path::{Component, Path};

use arm_harness::evidence::{ExitReason, RunRecord, RunSet, SCHEMA_VERSION, Stage, StepTransition};
use arm_harness::sys::BR_RETIRED_RAW;
use oracle_model::{ALL_PAYLOADS, Expectation, Payload, Scale, Weights, expected, trips};

/// A ceiling on the cumulative per-trip oracle work one `check_counts` may simulate.
/// `branch-dense`'s oracle iterates once per trip (~10⁸ at scale 1e8); a real AA-1 run
/// has a few distinct such inputs (~10⁹ trips total). Far above that, this exists only so
/// a hostile `records.jsonl` of many distinct large-scale `branch-dense` seeds cannot turn
/// grading into a multi-hour hang: over the ceiling, the checker fails CLOSED, not hung —
/// the same discipline as [`check_totality`]'s `attempted: u64::MAX` guard.
const MAX_ORACLE_TRIPS: u64 = 20_000_000_000;
use sha2::{Digest, Sha256};

use crate::error::LoadError;

/// The conventional manifest file name inside a run-set directory.
pub const MANIFEST_FILE: &str = "run-set.json";

/// The three `CLOCKPAGE mode=` tokens the guest can print
/// (`payloads/runtime/src/pvclock.rs`), in order of what AA-5 makes of each:
/// - `work-derived` — the harness published a page it refreshes from work. This, and
///   only this, is what AA-5 certifies.
/// - `managed-static` — the harness published a page, but a static placeholder (the
///   publication plumbing works; the work-derived refresh, `hm-8h8`, is not built). AA-5
///   reads this as unfulfilled, not a pass.
/// - `self-seeded` — the payload published its own static page because the harness never
///   did. The fallback; a hard fail at AA-5.
const WORK_DERIVED_CLOCKPAGE: &str = "work-derived";
const MANAGED_STATIC_CLOCKPAGE: &str = "managed-static";

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
    /// `--min-cases`: the run-set must contain at least this many DISTINCT armed
    /// target/seed cases (a case is one `(payload, scale, seed, condition, target_delta)`
    /// input; reps of it share the key). This binds the `cases` plan dimension SEPARATELY
    /// from the deadline total: without it, `--cases 1 --reps N` meets the ≥10⁶ armed floor
    /// by cloning a handful of targets, so the seeded-random target/skid distribution
    /// AA-1/AA-3 rest on is barely exercised. Absent for an armed AA-1/AA-3 run reads
    /// NOT-REQUESTED (never a silent pass); other stages have no case dimension.
    pub min_cases: Option<u64>,
    /// `--sub-normative`: permit a floor BELOW the stage's normative minimum (AA-1/AA-3's
    /// 1,000,000 armed overflows, AA-6's 1,000 repetitions). Off by default: a
    /// below-normative floor fails closed, so a weakened verdict cannot be produced by
    /// accident. When on, the run may pass a smaller floor, but every such outcome is
    /// marked `SUB-NORMATIVE` so it is never mistaken for a normative acceptance. This is
    /// for the checker's own fixtures and for dev runs, never a real disposition.
    pub sub_normative: bool,
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
    /// AA-6's run-set covers every required payload in the determinism matrix (the rep
    /// floor only grades inputs that are present, so a missing payload is otherwise
    /// invisible).
    Aa6Matrix,
    /// A cumulative (condition-matrix) check spans exactly one stage, with no duplicate
    /// run-sets double-counting the floor.
    Aggregation,
    /// Every record's condition matches its manifest (per run-set).
    ConditionConsistency,
    /// At AA-1, the cumulative run covers the required distinct contamination-condition
    /// matrix.
    ConditionMatrix,
    /// AA-4's LSE-only atomics contract: the structured evidence set (LSE-invariance under
    /// injection, LL/SC divergence, the three enforcement levels, the recorded ruling).
    Aa4Contract,
    /// At AA-1/AA-3, the armed deadlines span at least `--min-cases` DISTINCT target/seed
    /// cases — the `cases` plan dimension, bound separately from the deadline total.
    CaseCoverage,
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
            CheckId::Aa6Matrix => "aa6-matrix",
            CheckId::Aggregation => "aggregation",
            CheckId::ConditionConsistency => "condition-consistency",
            CheckId::ConditionMatrix => "condition-matrix",
            CheckId::Aa4Contract => "aa4-contract",
            CheckId::CaseCoverage => "case-coverage",
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
    let (run_set, records, records_bytes) = load_run_set(dir)?;

    let mut outcomes = Vec::new();
    run_stage_checks(&run_set, floors, &records, &records_bytes, &mut outcomes);
    // One run-set: the armed floor and case coverage are over this set's own overflows.
    check_armed_floor(run_set.stage, floors, count_armed(&records), &mut outcomes);
    check_case_coverage(
        run_set.stage,
        floors,
        distinct_armed_cases(&records).len() as u64,
        &mut outcomes,
    );

    Ok(CheckReport {
        run_set_id: run_set.run_set_id,
        stage: run_set.stage,
        outcomes,
    })
}

/// Every per-run-set check EXCEPT the armed-overflow floor. The armed floor is applied
/// by the caller — per-set for a single run-set ([`check_run_set`]), cumulative over the
/// union for a condition matrix ([`check_run_sets`]) — because AA-1's floor is cumulative
/// across the contamination conditions, each of which is its own run-set.
fn run_stage_checks(
    run_set: &RunSet,
    floors: &Floors,
    records: &[RunRecord],
    records_bytes: &[u8],
    out: &mut Vec<Outcome>,
) {
    check_schema_version(run_set, out);
    check_well_formed(run_set, records, out);
    check_records_sha256(run_set, records_bytes, out);
    check_totality(run_set, records, out);
    check_multiplicity(run_set, records, out);
    check_weights_and_counts(run_set, records, out);
    check_skid(run_set, records, out);
    check_mechanism(run_set, records, out);
    check_perf(run_set, records, out);
    check_image_pins(run_set, records, out);
    check_pinning(run_set, out);
    check_params_mode(records, out);
    check_clockpage_mode(run_set, records, out);
    check_replay_identity(run_set.stage, records, out);
    check_debug_evidence(run_set.stage, records, out);
    check_aa6_matrix(run_set.stage, records, out);
    check_aa4_contract(run_set.stage, records, out);
    check_condition_consistency(run_set, records, out);
    check_payload_status(records, out);
    check_rep_floor(run_set, floors, records, out);
}

/// Every record's `condition` must match its manifest's. A record labelled with a
/// condition its run-set did not sweep is either a mislabel or a spliced record — and a
/// condition-matrix verdict that trusts the manifest's condition while the records carry
/// another is not measuring what it claims. Runs on every set, single or aggregated.
fn check_condition_consistency(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut mismatched: Vec<(u64, &str)> = records
        .iter()
        .filter(|r| r.condition != run_set.condition)
        .map(|r| (r.sample_id, r.condition.as_str()))
        .collect();
    mismatched.sort_by_key(|&(id, _)| id);
    if mismatched.is_empty() {
        out.push(pass(
            CheckId::ConditionConsistency,
            format!(
                "every record's condition matches the manifest's ({})",
                run_set.condition
            ),
        ));
    } else {
        let shown: Vec<String> = mismatched
            .iter()
            .take(8)
            .map(|(id, c)| format!("sample {id}={c}"))
            .collect();
        out.push(fail(
            CheckId::ConditionConsistency,
            format!(
                "{} record(s) carry a condition other than the manifest's {:?}: {}",
                mismatched.len(),
                run_set.condition,
                shown.join(", ")
            ),
        ));
    }
}

/// Check a **condition matrix**: several run-set directories, one per contamination
/// condition, summed into a single stage verdict.
///
/// AA-1's million-overflow floor is *cumulative across the required condition matrix*,
/// not per-condition — so a run-set per condition must be summable. Each set is checked
/// on its own (every per-record floor, and its own rep floor), and the armed-overflow
/// floor is then applied ONCE over the union: a million `pinned-solo` overflows can no
/// longer pass while the contamination conditions went unmeasured, and several smaller
/// condition sets that together exceed the floor now can.
///
/// A **single** directory goes through the same path — NOT a shortcut to
/// [`check_run_set`] — precisely so a lone normative AA-1 run (a million `pinned-solo`
/// overflows in one directory) still meets the condition-matrix requirement rather than
/// bypassing it. This is the checker's acceptance entry point; the matrix is inherently
/// multi-condition, so a single condition can never satisfy it.
///
/// # Errors
/// [`LoadError`] if any directory's evidence is unreadable.
pub fn check_run_sets(dirs: &[&Path], floors: &Floors) -> Result<CheckReport, LoadError> {
    if dirs.is_empty() {
        // No directories: let the loader fail cleanly on a missing manifest.
        return check_run_set(Path::new("."), floors);
    }
    // Load every run-set up front; a single unreadable one fails the whole run.
    let mut loaded: Vec<(RunSet, Vec<RunRecord>, Vec<u8>)> = Vec::new();
    for dir in dirs {
        loaded.push(load_run_set(dir)?);
    }
    Ok(aggregate(&loaded, floors))
}

/// The AA-1 contamination condition matrix (`docs/ARM-ALTRA.md` §AA-1 contamination
/// probes): the baseline plus co-tenant load on other cores, on the same core, and under
/// memory pressure. AA-1's ≥10⁶ cumulative floor is over these DISTINCT conditions — a
/// million samples under one condition does not certify count invariance under
/// contamination. Arrival-day AA-1 runs use these canonical labels, one run-set each.
const REQUIRED_AA1_CONDITIONS: &[&str] = &[
    "pinned-solo",
    "co-tenant-other-core",
    "co-tenant-same-core",
    "memory-pressure",
];

/// The scales AA-1's **differential sweep** must cover. AA-1 derives the stable per-class
/// count offsets by measuring at 1e6, 1e7 and 1e8 and differencing — the offset is what is
/// left when the scale-proportional term is removed, and one scale cannot separate the two.
/// The CLI default is `smoke`; a smoke-only AA-1 run must therefore not certify the stage.
const REQUIRED_AA1_SWEEP_SCALES: &[Scale] = &[Scale::S1e6, Scale::S1e7, Scale::S1e8];

/// The payload classes AA-1 must characterize — the distinct `BR_RETIRED` behaviours whose
/// per-class offset it establishes: pure sequential (`straight-line`), branch-dense, a
/// synchronous exception (`svc`), a wait (`wfi-idle`), and an asynchronous abort
/// (`exception-abort`). A normative AA-1 verdict requires EVERY one of these measured at
/// EVERY contamination condition and EVERY sweep scale (a full grid), because a submission of
/// only `straight-line` never measures the WFI/exception classes, and disjoint payloads per
/// condition give no per-class contamination comparison.
const REQUIRED_AA1_PAYLOADS: &[Payload] = &[
    Payload::StraightLine,
    Payload::BranchDense,
    Payload::Svc,
    Payload::WfiIdle,
    Payload::ExceptionAbort,
];

/// The cumulative verdict over a set of already-loaded run-sets. Factored from the disk
/// loading so the aggregation rules — one stage, no duplicates, the condition matrix, the
/// summed floor — are unit-testable without fixtures on disk.
fn aggregate(loaded: &[(RunSet, Vec<RunRecord>, Vec<u8>)], floors: &Floors) -> CheckReport {
    let stage = loaded[0].0.stage;
    let mut outcomes = Vec::new();

    check_aggregation(loaded, stage, &mut outcomes);
    check_aa1_condition_matrix(loaded, stage, floors, &mut outcomes);

    // Every per-set check runs on each set (its records, its rep floor, its own
    // record-vs-manifest condition consistency). The armed floor is deferred to the
    // cumulative check below.
    for (rs, records, bytes) in loaded {
        run_stage_checks(rs, floors, records, bytes, &mut outcomes);
    }

    // The cumulative armed-overflow floor, over every condition's overflows.
    let total_armed: u64 = loaded.iter().map(|(_, r, _)| count_armed(r)).sum();
    check_armed_floor(stage, floors, total_armed, &mut outcomes);

    // Cumulative distinct-case coverage: the UNION of armed cases across every condition. A
    // case that recurs in two conditions is still one case, so the set is unioned across the
    // aggregate rather than summing per-set counts.
    let mut cases: BTreeSet<CaseKey> = BTreeSet::new();
    for (_, records, _) in loaded {
        cases.extend(distinct_armed_cases(records));
    }
    check_case_coverage(stage, floors, cases.len() as u64, &mut outcomes);

    let run_set_id = if let [(rs, _, _)] = loaded {
        rs.run_set_id.clone()
    } else {
        let ids: Vec<&str> = loaded
            .iter()
            .map(|(rs, _, _)| rs.run_set_id.as_str())
            .collect();
        format!("aggregate[{}]", ids.join(" + "))
    };
    CheckReport {
        run_set_id,
        stage,
        outcomes,
    }
}

/// The aggregation must be one stage, with **no duplicate run-sets** — summing the same
/// evidence twice (same id, or bit-identical records) would inflate the cumulative floor
/// without a single additional measurement (a 500,000-record set supplied twice would
/// "meet" the million-overflow floor).
fn check_aggregation(
    loaded: &[(RunSet, Vec<RunRecord>, Vec<u8>)],
    stage: Stage,
    out: &mut Vec<Outcome>,
) {
    let mut problems: Vec<String> = Vec::new();
    for (rs, _, _) in loaded {
        if rs.stage != stage {
            problems.push(format!(
                "run-set {} is stage {:?}, not {stage:?} — a cumulative verdict is over one \
                 stage, not a mix",
                rs.run_set_id, rs.stage
            ));
        }
    }
    let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
    let mut seen_hashes: BTreeSet<String> = BTreeSet::new();
    for (rs, _, _) in loaded {
        if !seen_ids.insert(rs.run_set_id.as_str()) {
            problems.push(format!(
                "run-set id {} appears more than once — summing the same evidence twice inflates \
                 the cumulative floor without a new measurement",
                rs.run_set_id
            ));
        }
        // Deduplicate on the SAME normalized hash `check_records_sha256` verifies against
        // (`sha256:` prefix dropped, lowercased), or the same records supplied twice —
        // once as `<hex>`, once as `sha256:<hex>` — would each verify yet count as
        // distinct evidence and double the cumulative floor. Truncate the preview
        // char-safely: `records_sha256` is untrusted, and a byte index mid-code-point
        // would panic before the well-formed check can reject the malformed hash.
        if !seen_hashes.insert(normalise_hash(&rs.records_sha256)) {
            problems.push(format!(
                "two run-sets carry identical records (records_sha256 {}…) — the same overflows \
                 counted twice",
                rs.records_sha256.chars().take(16).collect::<String>()
            ));
        }
    }

    // Comparability. Summing records across conditions and grading count invariance is only
    // meaningful if every set was measured under ONE constants pack and ONE measurement
    // environment — the sweep varies the `condition`, nothing else. If the sets differ in
    // `weights`, `perf`, `environment`, or `mechanism`, a condition-dependent count change can
    // be absorbed by a compensating per-set difference (a different count offset in each
    // `weights` pack, say) and the aggregate still "passes" count invariance. So require those
    // four to match the first set. `condition` is the sweep and is not compared. `pinning` is
    // part of the measurement environment, but its ONE sanctioned exception is per-SET, not
    // per-stage: only AA-1's migration probe (the one set that runs unpinned by design) may
    // differ; every normal set — AA-1's ordinary condition sets included — must share one
    // posture, checked against a non-probe reference below. The workload identity is the set
    // of image content hashes (paths are never trusted).
    let image_ids = |rs: &RunSet| -> Vec<String> {
        let mut v: Vec<String> = rs
            .images
            .iter()
            .map(|i| normalise_hash(&i.sha256))
            .collect();
        v.sort();
        v
    };
    if let [(first, _, _), rest @ ..] = loaded {
        for (rs, _, _) in rest {
            let mut diffs: Vec<&str> = Vec::new();
            if rs.weights != first.weights {
                diffs.push("weights");
            }
            if rs.perf != first.perf {
                diffs.push("perf");
            }
            if rs.environment != first.environment {
                diffs.push("environment");
            }
            if rs.mechanism != first.mechanism {
                diffs.push("mechanism");
            }
            // The PINNED PAYLOAD IMAGES must match too: if the binaries changed between
            // conditions, an apparent condition-dependent count difference (or invariance)
            // could be an artefact of a different measured workload, not the contamination.
            if image_ids(rs) != image_ids(first) {
                diffs.push("images");
            }
            if !diffs.is_empty() {
                problems.push(format!(
                    "run-set {} differs from {} in [{}] — aggregated conditions must share one \
                     constants pack (weights), measurement environment (perf, environment, \
                     mechanism), and payload images; only `condition` (and AA-1's migration-probe \
                     pinning) may vary, or a condition-dependent count change hides behind a \
                     per-set difference",
                    rs.run_set_id,
                    first.run_set_id,
                    diffs.join(", ")
                ));
            }
        }
    }

    // Pinning is the PMU/skid measurement environment: two sets pinned to DIFFERENT cores (or
    // under different governors) are a different measurement, not one summed population, so
    // NORMAL sets must share one pinning posture. The ONE exception is AA-1's sanctioned
    // migration probe, which runs UNPINNED by design — so it is exempt, while every other set
    // (AA-1's ordinary condition sets included) is held to the first non-probe set's posture.
    // Comparing against a non-probe reference (not simply the first set) is what stops a probe
    // that happens to sort first from letting the normal sets diverge. Outside AA-1 there is no
    // probe, so this reduces to "all sets share pinning".
    if let Some(reference) = loaded
        .iter()
        .map(|(rs, _, _)| rs)
        .find(|rs| !rs.pinning.migration_probe)
    {
        for (rs, _, _) in loaded {
            if !rs.pinning.migration_probe && rs.pinning != reference.pinning {
                problems.push(format!(
                    "run-set {} pins to a different posture (pinned {:?}, core {:?}, governor {}) \
                     than the aggregate ({} — pinned {:?}, core {:?}, governor {}): normal sets \
                     must share ONE pinned core and governor; only AA-1's migration probe may \
                     run unpinned",
                    rs.run_set_id,
                    rs.pinning.pinned,
                    rs.pinning.core,
                    rs.pinning.governor,
                    reference.run_set_id,
                    reference.pinning.pinned,
                    reference.pinning.core,
                    reference.pinning.governor,
                ));
            }
        }
    }

    // AA-1's contract permits exactly ONE bounded migration probe, then PERMANENT repinning.
    // More than one probe, or an aggregate that is ENTIRELY probes (no repinned measurement),
    // is a fully-unpinned matrix masquerading as a certification — and the all-probe case also
    // leaves the posture check above with no non-probe reference, silently skipping it.
    if stage == Stage::Aa1 {
        let probes: Vec<&str> = loaded
            .iter()
            .filter(|(rs, _, _)| rs.pinning.migration_probe)
            .map(|(rs, _, _)| rs.run_set_id.as_str())
            .collect();
        if probes.len() > 1 {
            problems.push(format!(
                "AA-1 aggregate carries {} migration-probe run-sets ({}) — the contract permits \
                 exactly one bounded probe, then permanent repinning",
                probes.len(),
                probes.join(", ")
            ));
        }
        if !loaded.is_empty() && loaded.iter().all(|(rs, _, _)| rs.pinning.migration_probe) {
            problems.push(
                "every AA-1 run-set is a migration probe — a fully unpinned matrix. AA-1 permits \
                 one bounded probe amid PINNED measurement, not an all-unpinned population"
                    .to_string(),
            );
        }
    }

    verdict(
        CheckId::Aggregation,
        &problems,
        "one stage, distinct run-sets, one constants pack + measurement environment",
        out,
    );
}

/// At AA-1, an **armed** cumulative run must cover the required distinct contamination
/// conditions ([`REQUIRED_AA1_CONDITIONS`]) — the ≥10⁶ armed-overflow floor is over the
/// matrix, so a single condition (even a million overflows of it) does not certify count
/// invariance under contamination. Fires only at AA-1 and only when overflows were armed:
/// AA-1(b) counting mode has no armed floor and so no matrix requirement.
///
/// Coverage is by **measurement, not by label**. Requiring only that each condition's
/// name appears would let a million-overflow `pinned-solo` set ride beside three
/// *counting-mode* (zero-armed) run-sets carrying the other labels: the labels are all
/// present, the cumulative armed floor is met by `pinned-solo` alone, yet the three
/// contamination conditions were never actually exercised under armed overflow. So each
/// required condition must contribute a NONZERO armed count, and — for a normative run —
/// at least its equal share of the requested floor.
fn check_aa1_condition_matrix(
    loaded: &[(RunSet, Vec<RunRecord>, Vec<u8>)],
    stage: Stage,
    floors: &Floors,
    out: &mut Vec<Outcome>,
) {
    if stage != Stage::Aa1 {
        return;
    }
    // Armed overflows contributed under each condition (summed across its run-sets). The
    // MIGRATION PROBE is excluded: it is a bounded, unpinned experiment of its own, so its
    // armed records must not count toward the pinned condition matrix — otherwise a fully
    // unpinned probe could satisfy a required contamination condition it never measured under
    // the pinned posture AA-1's count invariance is about.
    let mut armed_by_condition: BTreeMap<&str, u64> = BTreeMap::new();
    for (rs, records, _) in loaded {
        if rs.pinning.migration_probe {
            continue;
        }
        *armed_by_condition.entry(rs.condition.as_str()).or_default() += count_armed(records);
    }
    let total_armed: u64 = armed_by_condition.values().sum();
    if total_armed == 0 {
        // AA-1(b) counting mode: no armed floor, so no contamination-matrix requirement.
        return;
    }

    // The per-condition share of the requested floor, enforced only for a normative run
    // (a --sub-normative test floor relaxes the magnitude, as it does for the floors
    // themselves — but every required condition must still be measured at all).
    let n = REQUIRED_AA1_CONDITIONS.len() as u64;
    let share = if floors.sub_normative {
        0
    } else {
        floors.min_armed_overflows.unwrap_or(0) / n
    };

    let mut problems: Vec<String> = Vec::new();

    // The full AA-1 grid: every required payload class × every contamination condition ×
    // every sweep scale must carry an armed record. A union-of-labels check (each scale
    // occurs SOMEWHERE, each condition has SOME armed record) is not enough: a submission of
    // only `straight-line` at three scales under four conditions leaves the WFI/exception
    // classes unmeasured, and disjoint payloads per condition give no per-class contamination
    // comparison. Requiring the explicit cross-product closes both. Normative only —
    // `--sub-normative` relaxes it as it relaxes the floor magnitude.
    if !floors.sub_normative {
        let mut armed_cells: BTreeSet<(&str, &str, Scale)> = BTreeSet::new();
        for (rs, records, _) in loaded {
            // The migration probe is excluded from grid coverage for the same reason: its
            // unpinned armed records do not measure a pinned matrix cell.
            if rs.pinning.migration_probe {
                continue;
            }
            for r in records {
                if r.overflow.as_ref().is_some_and(|o| o.armed) {
                    armed_cells.insert((r.payload.name(), r.condition.as_str(), r.scale));
                }
            }
        }
        let mut missing_cells: Vec<String> = Vec::new();
        for payload in REQUIRED_AA1_PAYLOADS {
            for &cond in REQUIRED_AA1_CONDITIONS {
                for &scale in REQUIRED_AA1_SWEEP_SCALES {
                    if !armed_cells.contains(&(payload.name(), cond, scale)) {
                        missing_cells.push(format!("{}×{cond}×{}", payload.name(), scale.name()));
                    }
                }
            }
        }
        if !missing_cells.is_empty() {
            let shown = missing_cells
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let suffix = if missing_cells.len() > 8 {
                format!(" (+{} more)", missing_cells.len() - 8)
            } else {
                String::new()
            };
            problems.push(format!(
                "the AA-1 matrix is incomplete: {} required (payload × condition × scale) cell(s) \
                 have no armed record — {shown}{suffix}. AA-1 must measure every characterized \
                 class at every contamination condition and every sweep scale; the union of \
                 labels, or disjoint payloads per condition, gives no per-class differential and \
                 no contamination comparison (pass --sub-normative for a reduced-scope run)",
                missing_cells.len()
            ));
        }

        // The bounded migration probe is a REQUIRED AA-1 certification input, not an
        // optional extra: the four pinned contamination conditions never leave their core,
        // so they cannot supply the cross-core-migration evidence (the rr #3607 missed-PMI
        // failure mode) the stage contract calls for. And the probe must have run UNDER AN
        // ARMED OVERFLOW — a counting-mode probe set migrates nothing that could miss a PMI,
        // so a manifest merely LABELLED `migration_probe: true` while all armed records come
        // from the pinned sets does not cover the experiment. So the probe set(s) must
        // themselves contribute armed overflows. (`check_pinning` separately proves that
        // probe is genuinely unpinned.)
        let probe_armed: u64 = loaded
            .iter()
            .filter(|(rs, _, _)| rs.pinning.migration_probe)
            .map(|(_, records, _)| count_armed(records))
            .sum();
        if probe_armed == 0 {
            problems.push(
                "no armed overflow came from an AA-1 migration-probe run-set \
                 (pinning.migration_probe with armed records): the cross-core-migration \
                 evidence (rr #3607) must be measured UNDER an armed overflow, and neither an \
                 absent probe nor a counting-mode one labelled as the probe supplies it — the \
                 pinned conditions never migrate (pass --sub-normative for a reduced-scope run)"
                    .to_string(),
            );
        }
    }

    for &cond in REQUIRED_AA1_CONDITIONS {
        let armed = armed_by_condition.get(cond).copied().unwrap_or(0);
        if armed == 0 {
            problems.push(format!(
                "condition {cond} contributed 0 armed overflows (absent, or a counting-mode run): \
                 count invariance under it was never measured — the cumulative floor is met by the \
                 other conditions, which is exactly what the matrix forbids"
            ));
        } else if armed < share {
            problems.push(format!(
                "condition {cond} contributed only {armed} armed overflows, below its {share} \
                 share of the {} normative floor (pass --sub-normative for a reduced-scope run)",
                floors.min_armed_overflows.unwrap_or(0)
            ));
        }
    }
    verdict(
        CheckId::ConditionMatrix,
        &problems,
        format!(
            "the AA-1 contamination matrix is covered and each condition measured: {}",
            REQUIRED_AA1_CONDITIONS.join(", ")
        ),
        out,
    );
}

/// Whether a manifest's `records_file` is a plain relative path that stays INSIDE its
/// run-set directory: non-empty, and built only of normal (or `.`) components — no `..`,
/// no absolute/root/prefix. `Path::join` follows an absolute path or `..` out of the base,
/// so anything else would read evidence from outside the retained package.
fn records_file_is_confined(records_file: &str) -> bool {
    !records_file.is_empty()
        && Path::new(records_file)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Load one run-set (manifest + records + raw record bytes) from a directory.
fn load_run_set(dir: &Path) -> Result<(RunSet, Vec<RunRecord>, Vec<u8>), LoadError> {
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
    // Confine records_file to the run-set directory BEFORE touching the filesystem.
    // `records_file` is untrusted, and `Path::join` follows an absolute path or `..`
    // components straight out of `dir` — so an unconfined path would let a manifest pass
    // every hash and floor check on records outside the retained package (or an arbitrary
    // external file).
    if !records_file_is_confined(&run_set.records_file) {
        return Err(LoadError::RecordsPathEscapesDir {
            dir: dir.to_path_buf(),
            records_file: run_set.records_file.clone(),
        });
    }
    let records_path = dir.join(&run_set.records_file);
    // The lexical check rejects `..`/absolute in the manifest STRING, but a symlinked
    // records.jsonl INSIDE the directory can still resolve outside it — and `std::fs::read`
    // follows the link. Canonicalize both the directory and the records path (resolving every
    // symlink) and require the real records file to stay beneath the real directory before
    // reading a byte. Both are canonicalized so a symlinked ancestor of `dir` itself (e.g.
    // `/tmp` → `/private/tmp`) does not spuriously fail the containment test.
    let canon_dir = dir
        .canonicalize()
        .map_err(|source| LoadError::ReadRecords {
            path: dir.to_path_buf(),
            source,
        })?;
    let canon_records = records_path
        .canonicalize()
        .map_err(|source| LoadError::ReadRecords {
            path: records_path.clone(),
            source,
        })?;
    if !canon_records.starts_with(&canon_dir) {
        return Err(LoadError::RecordsPathEscapesDir {
            dir: canon_dir,
            records_file: run_set.records_file.clone(),
        });
    }
    let records_bytes = std::fs::read(&canon_records).map_err(|source| LoadError::ReadRecords {
        path: canon_records.clone(),
        source,
    })?;
    let records = parse_records(&canon_records, &records_bytes)?;
    Ok((run_set, records, records_bytes))
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
        // Strip ONLY the optional lowercase `sha256:` prefix — do NOT lowercase the digits.
        // The schema pattern is `[0-9a-f]{64}`, so an UPPERCASE hash is schema-invalid; but
        // `normalise_hash` (used for the case-insensitive records-hash MATCH) would lowercase
        // it first and let it pass this FORMAT check. Well-formedness is about the canonical
        // form, so validate the raw digits.
        let raw = h.strip_prefix("sha256:").unwrap_or(h);
        if !is_lower_hex(raw, 64) {
            problems.push(format!("{what} is not a 64-lowercase-hex sha256: {h:?}"));
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

    // Every record's condition is non-empty, and its state_digest is present (schema
    // `minLength: 1`). The digest matters even for armed/stepped records that also carry a
    // landed/step digest: replay identity compares the mode-specific `comparison_digest`,
    // so an EMPTY `state_digest` would slip past that check while still violating the
    // schema and lacking the advertised complete-state evidence. Enforce it here, on every
    // record, not just the counting ones.
    for r in records {
        if r.condition.trim().is_empty() {
            problems.push(format!("record {}: condition is empty", r.sample_id));
        }
        if r.state_digest.trim().is_empty() {
            problems.push(format!(
                "record {}: state_digest is empty (schema minLength 1) — every record must carry \
                 its complete-state digest, even one that also carries a landed or step digest",
                r.sample_id
            ));
        }
        // Step and armed-overflow are MUTUALLY EXCLUSIVE modes: a record is EITHER an AA-2
        // single-step OR an armed landing, never both. The schema permits both Option fields,
        // but a record carrying both is two mechanisms wearing one name — and it would let a
        // crafted record's `step_digest` stand in for replay while its divergent LANDED digests
        // went unchecked. Reject it here rather than letting comparison_digest pick.
        if r.step.is_some() && r.overflow.as_ref().is_some_and(|o| o.armed) {
            problems.push(format!(
                "record {}: carries BOTH a step and an armed overflow — a record is a \
                 single-step OR an armed landing, never both",
                r.sample_id
            ));
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
    // A run-set that attempted NOTHING measures nothing. With `attempted: 0` and an
    // empty (correctly hashed) records file, every per-record and totality check passes
    // vacuously — the checker would certify a run that never happened. The harness
    // already refuses an empty plan (`--reps 0`); the checker refuses it independently,
    // and the schema pins `attempted` ≥ 1 (`run-set.schema.json`).
    if attempted == 0 {
        out.push(fail(
            CheckId::Totality,
            "the run-set attempted 0 samples: an empty plan measures nothing, and a verdict \
             over it certifies a run that never happened",
        ));
        return;
    }
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

fn check_multiplicity(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
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

    // The migration probe is the ONE unpinned set, and its whole purpose is to RECORD the
    // rr #3607 lost-PMI signature: an armed overflow that a cross-core migration caused KVM to
    // MISS (deliveries == 0). Grading that as a multiplicity FAILURE — the pinned matrix's
    // exactly-once requirement — would make the probe unable to record the very signature it
    // exists for, an internal contradiction with the pinned matrix passing beside it. So for
    // the probe, deliveries == 0 is the EXPECTED signature (reported, not failed); only a
    // DUPLICATE delivery (> 1) is anomalous. The pinned sets keep the strict exactly-once rule.
    if run_set.pinning.migration_probe {
        if duplicated.is_empty() {
            out.push(pass(
                CheckId::Multiplicity,
                format!(
                    "migration probe: {armed} armed overflow(s), {} lost to migration \
                     (deliveries == 0 — the recorded rr #3607 missed-PMI signature), none \
                     duplicated",
                    lost.len()
                ),
            ));
        } else {
            out.push(fail(
                CheckId::Multiplicity,
                format!(
                    "migration probe: duplicate deliveries (> 1) at samples {} — a missed PMI is \
                     the expected signature, but a DOUBLE delivery is not",
                    preview(duplicated.iter().copied())
                ),
            ));
        }
        return;
    }

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
    // Cumulative per-trip work of the NEW (non-memoized) oracle computations, bounded so
    // untrusted records cannot force an unbounded simulation (`MAX_ORACLE_TRIPS`).
    let mut oracle_trips: u64 = 0;

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

        let key = (r.payload, r.scale, r.seed);
        let e = if let Some(e) = oracle.get(&key) {
            *e
        } else {
            // A NEW oracle computation. `branch-dense` iterates once per trip; every
            // other payload is O(1). Bound the CUMULATIVE trips of the distinct inputs so
            // a hostile records file of many large-scale `branch-dense` seeds fails closed
            // rather than hanging the checker for hours.
            if r.payload == Payload::BranchDense {
                oracle_trips = oracle_trips.saturating_add(trips(r.payload, r.scale));
                if oracle_trips > MAX_ORACLE_TRIPS {
                    problems.push(format!(
                        "grading these records would force the oracle to simulate over \
                         {MAX_ORACLE_TRIPS} branch-dense trips — a records file with many distinct \
                         large-scale seeds. Refusing to grade unboundedly: fail closed, not hung"
                    ));
                    break;
                }
            }
            let e = expected(r.payload, r.scale, r.seed);
            oracle.insert(key, e);
            e
        };

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

        // The oracle prediction is CHECKED arithmetic: `None` means the (weights, reported
        // term) would overflow u64. That is malformed evidence — a saturated u64::MAX
        // prediction would be matched by a record whose own measured_taken is u64::MAX — so
        // it fails closed rather than being read as a valid oracle count.
        match e.total(weights, r.reported_taken) {
            None => problems.push(format!(
                "sample {}: payload {} scale {} seed {}: the oracle prediction overflows u64 \
                 (weights or reported term unrepresentably large) — malformed evidence, refused",
                r.sample_id,
                r.payload.name(),
                r.scale.name(),
                r.seed
            )),
            Some(predicted) if predicted != r.measured_taken => problems.push(format!(
                "sample {}: payload {} scale {} seed {}: oracle predicts {predicted} \
                 taken branches but the record measured {}",
                r.sample_id,
                r.payload.name(),
                r.scale.name(),
                r.seed,
                r.measured_taken
            )),
            Some(_) => {}
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

    // AA-1's armed skid measurement is AA-1(c): the PRE-PATCH host signal kick
    // (`ExitReason::SignalKick`). AA-3 replaces that with the in-kernel force-exit
    // (`Preempt`), which has different delivery and skid behaviour — so an AA-1 run armed
    // through the patched Preempt path is measuring a different mechanism than the stage is
    // about, and cannot certify it. Only AA-3/AA-4/AA-6 get the patched constraint above;
    // without this, `--mechanism patched` at AA-1 produces a self-consistent Preempt tuple
    // that no stage-specific check rejects. A counting-mode AA-1 run (AA-1(a)/(b)) arms
    // nothing and ends at the console sentinel, so this is scoped to armed runs.
    let any_armed = records
        .iter()
        .any(|r| r.overflow.as_ref().is_some_and(|o| o.armed));
    if stage == Stage::Aa1 && any_armed && m.expected_exit_reason != ExitReason::SignalKick {
        problems.push(format!(
            "stage AA-1 armed overflows but declares expected_exit_reason={:?}: AA-1(c) measures \
             the pre-patch host signal kick (signal-kick), which the in-kernel Preempt path \
             (AA-3's) replaces with different delivery and skid behaviour. An AA-1 skid \
             measurement must run the stock signal-kick mechanism, not the patched force-exit",
            m.expected_exit_reason
        ));
    }

    // Every record that ARMED an overflow must carry the claimed mechanism exit.
    // This is the other half: a run that silently exercised the stock signal-kick
    // path while claiming the patched Preempt exit fails here.
    //
    // `expected_exit_reason` describes the armed LANDING, so the comparison is scoped to armed
    // records that actually DELIVERED (`deliveries >= 1`) — the only ones with a mechanism exit
    // to attest. Two record classes legitimately end at the console sentinel with
    // `ExitReason::Mmio` and must NOT be graded here: a count-only run that armed nothing
    // (AA-1(b), `--with-targets` absent), and a LOST PMI (armed but `deliveries == 0` — the
    // rr #3607 signature the migration probe records). Grading the lost PMI's sentinel exit as a
    // mechanism mismatch would contradict r24's multiplicity carve-out, which accepts exactly
    // that signature for the probe; the delivery count is graded by `check_multiplicity`, not
    // here.
    let mut mismatched: Vec<(u64, ExitReason)> = Vec::new();
    for r in records {
        let delivered = r
            .overflow
            .as_ref()
            .is_some_and(|o| o.armed && o.deliveries >= 1);
        if delivered && r.exit_reason != m.expected_exit_reason {
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

/// The final path component of an image pin's `path` — the artifact's file name, which the
/// harness sets to the payload class name (`<payload_dir>/<name>`).
fn image_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path).trim()
}

fn check_image_pins(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    let mut problems: Vec<String> = Vec::new();

    if run_set.images.is_empty() {
        problems.push("the manifest pins no boot artifacts: nothing was attested".to_string());
    }
    // §Evidence integrity #3: a hash recorded but not verified before boot is the anti-pattern.
    let unverified: Vec<&str> = run_set
        .images
        .iter()
        .filter(|i| !i.verified_before_boot)
        .map(|i| i.path.as_str())
        .collect();
    if !unverified.is_empty() {
        problems.push(format!(
            "{} boot artifact(s) recorded a hash but were not verified before boot: {}",
            unverified.len(),
            unverified.join(", ")
        ));
    }

    // The host kernel is an exercised boot artifact: a VERIFIED image pin's hash must equal
    // mechanism.host_kernel_sha256, binding the kernel identity in the mechanism block to a
    // pin rather than letting it be a free-floating string.
    let kernel_hash = normalise_hash(&run_set.mechanism.host_kernel_sha256);
    if !kernel_hash.is_empty()
        && !run_set
            .images
            .iter()
            .any(|i| i.verified_before_boot && normalise_hash(&i.sha256) == kernel_hash)
    {
        problems.push(format!(
            "no verified image pin matches mechanism.host_kernel_sha256 ({}…): the host kernel \
             is an exercised boot artifact and must be pinned and verified, not merely named",
            kernel_hash.chars().take(16).collect::<String>()
        ));
    }

    // Every EXERCISED payload class must have its own verified image pin — the check used to
    // pass on a single verified image, so a run of every class (including LinuxGuest) could
    // retain only the host-kernel pin. The harness pins one image per loaded payload with the
    // class name as the file name.
    let mut exercised: Vec<&str> = records.iter().map(|r| r.payload.name()).collect();
    exercised.sort_unstable();
    exercised.dedup();
    let mut unpinned: Vec<&str> = exercised
        .iter()
        .copied()
        .filter(|name| {
            !run_set
                .images
                .iter()
                .any(|i| i.verified_before_boot && image_file_name(&i.path) == *name)
        })
        .collect();
    unpinned.sort_unstable();
    if !unpinned.is_empty() {
        problems.push(format!(
            "{} exercised payload class(es) have no verified image pin identifying them: {} — \
             every exercised boot/payload artifact must be pinned by content, not inferred from \
             one image standing in for all",
            unpinned.len(),
            unpinned.join(", ")
        ));
    }

    verdict(
        CheckId::ImagePins,
        &problems,
        format!(
            "all {} boot artifacts verified, the host kernel pinned, every exercised artifact \
             bound",
            run_set.images.len()
        ),
        out,
    );
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

    // The sanctioned migration probe is deliberately UNPINNED — its whole purpose is to
    // exercise the rr #3607 cross-core-migration failure mode. A `migration_probe: true`
    // that is nonetheless `pinned` (or carries a pinned core) never migrates, so it cannot
    // supply that evidence: the tuple is self-contradictory and is refused rather than
    // read as a normal pinning pass (which the `if p.pinned` branch below would do first).
    if p.migration_probe && (p.pinned || p.core.is_some()) {
        out.push(fail(
            CheckId::Pinning,
            format!(
                "migration_probe is set but the run is pinned (pinned={}, core={:?}): the \
                 migration probe is unpinned BY DESIGN to exercise the rr #3607 migration \
                 failure mode. A pinned probe never migrates, so this contradictory tuple \
                 cannot claim migration evidence — require pinned=false and no pinned core",
                p.pinned, p.core
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
/// The [`Payload::ClockPage`] payload AND the AA-5 [`Payload::LinuxGuest`] are the
/// **clock-attesting** records: each must evidence the harness's work-derived clock. The
/// other seven windowed payloads legitimately carry `clockpage_mode: None` and are excluded
/// (grading them would reject the standard matrix), but the Linux guest is NOT — a guest that
/// merely BOOTED is not evidence it consumed work-derived time, so its `clockpage_mode` is
/// graded exactly like the clock-page payload's. The mechanism itself must be exercised: at
/// least one clock-page record must exist, or AA-5 tested it not at all.
fn check_clockpage_mode(run_set: &RunSet, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if run_set.stage != Stage::Aa5 {
        return;
    }

    // The records whose clock attestation AA-5 grades: the clock-page payload and the Linux
    // guest. Presence of a guest record is not evidence — its mode is graded like the rest.
    let clock_attesting: Vec<&RunRecord> = records
        .iter()
        .filter(|r| matches!(r.payload, Payload::ClockPage | Payload::LinuxGuest))
        .collect();
    if !records.iter().any(|r| r.payload == Payload::ClockPage) {
        out.push(fail(
            CheckId::ClockPageMode,
            "AA-5 run-set contains no clock-page records: the paravirt-clock mechanism AA-5 \
             certifies was never exercised",
        ));
        return;
    }

    let mode_of = |r: &RunRecord| {
        r.clockpage_mode
            .clone()
            .unwrap_or_else(|| "none".to_string())
    };

    // A `self-seeded` (or absent) mode is a hard fail: the harness never published a
    // page, so the guest fell back to its own static one — AA-5's forbidden fallback.
    let mut fallback: Vec<(u64, String)> = clock_attesting
        .iter()
        .filter(|r| {
            !matches!(
                r.clockpage_mode.as_deref(),
                Some(WORK_DERIVED_CLOCKPAGE) | Some(MANAGED_STATIC_CLOCKPAGE)
            )
        })
        .map(|r| (r.sample_id, mode_of(r)))
        .collect();
    fallback.sort_by_key(|&(id, _)| id);
    if !fallback.is_empty() {
        let shown: Vec<String> = fallback
            .iter()
            .take(8)
            .map(|(id, mode)| format!("sample {id}={mode}"))
            .collect();
        out.push(fail(
            CheckId::ClockPageMode,
            format!(
                "{} AA-5 clock-attesting record(s) (clock-page/linux-guest) show a \
                 self-seeded/absent clock page: {} — the payload's own static fallback, not a \
                 harness-published page. AA-5 certifies the harness's clock.",
                fallback.len(),
                shown.join(", ")
            ),
        ));
        return;
    }

    // Every clock-attesting record is at least `managed-static`. AA-5 acceptance needs
    // `work-derived`; a static placeholder proves only the publication plumbing. If any is
    // static, the run does not certify AA-5 — but this is the accepted silicon-day deferral
    // (the work-derived refresh is `hm-8h8`'s), so it reads NOT-REQUESTED, not a fail, and not
    // a pass.
    let static_reps = clock_attesting
        .iter()
        .filter(|r| r.clockpage_mode.as_deref() == Some(MANAGED_STATIC_CLOCKPAGE))
        .count();
    if static_reps > 0 {
        out.push(not_requested(
            CheckId::ClockPageMode,
            format!(
                "{static_reps} of {} clock-attesting record(s) attest a \
                 `{MANAGED_STATIC_CLOCKPAGE}` page: the harness published it (the plumbing \
                 works), but it is a static placeholder, not the work-derived, refreshed clock \
                 AA-5 certifies. That mechanism is `hm-8h8` (docs/PARAVIRT-CLOCK.md) — a \
                 silicon-day item; this verdict cannot accept AA-5 until a \
                 `{WORK_DERIVED_CLOCKPAGE}` page exists.",
                clock_attesting.len()
            ),
        ));
    } else {
        out.push(pass(
            CheckId::ClockPageMode,
            format!(
                "all {} clock-attesting record(s) (clock-page + linux-guest) attest the \
                 {WORK_DERIVED_CLOCKPAGE} clock page (AA-5)",
                clock_attesting.len()
            ),
        ));
    }
}

/// The key a repetition is *the same run* under: same payload, scale, seed,
/// condition, the same target **delta** (armed runs), and the same **step moment**
/// (stepped AA-2 runs).
///
/// The delta is `target - work_begin`, NOT the absolute target. The plan reuses one
/// `target_delta` across every repetition of an input, but the stored target is
/// `work_begin + delta`; if pre-window execution diverges and `work_begin` differs,
/// the absolute targets differ and same-input records split into different groups —
/// so replay-identity would report "no repeated group" instead of catching the
/// divergent landed states. Keying by delta groups them correctly. A malformed
/// record where `target < work_begin` (a negative delta) is caught separately by
/// [`check_replay_identity`].
///
/// The step moment (`pc_before`, transition) is what makes an AA-2 stepped record a
/// distinct experiment: two step points of the SAME input — an `SVC` exception entry and
/// its `ERET` — share payload/scale/seed/condition and carry no target, so without it they
/// group together and their necessarily-different `step_digest`s read as false divergence.
/// Keying by the step moment groups each point with its own repetitions instead.
type RepKey = (
    String,
    String,
    u64,
    String,
    Option<i128>,
    Option<(u64, StepTransition)>,
);

fn rep_key(r: &RunRecord) -> RepKey {
    (
        r.payload.name().to_string(),
        r.scale.name().to_string(),
        r.seed,
        r.condition.clone(),
        r.overflow
            .as_ref()
            .map(|o| i128::from(o.target) - i128::from(r.work_begin)),
        r.step.as_ref().map(|s| (s.pc_before, s.transition)),
    )
}

/// The digest a repetition is compared on.
///
/// For a stepped AA-2 record, that is the **step-moment** digest — the state at the
/// Moment the single step landed. AA-2's acceptance is replay-identical *stepped* states,
/// and the final `state_digest` (taken at the exit sentinel) can converge: two divergent
/// step states can run on to the same final state, so it cannot establish step identity.
/// For an armed AA-3 landing it is the **landed** digest, for the same reason; for an
/// unarmed counting run there is no landing and the final state is the thing to compare.
///
/// It is STAGE-AWARE: an armed AA-3 landing compares the state AT THE LANDING (the injection
/// Moment), but AA-6 compares the FINAL `state_digest` — its acceptance is that the whole
/// event, INCLUDING processing the injected interrupt, is replay-identical, and two reps can
/// land identically at the injection Moment then diverge handling the event. Comparing
/// `landed_digest` for AA-6 would pass that divergence.
fn comparison_digest(stage: Stage, r: &RunRecord) -> &str {
    // An armed, delivered LANDING compares the landed state — EXCEPT at AA-6, whose acceptance
    // is that the whole event (interrupt processing included) replays, so it compares the final
    // `state_digest`. This takes precedence over a `step` field: a record's acceptance mode is
    // decided by its overflow, never diverted to `step_digest` because a step is also present.
    // (A record carrying BOTH a step and an armed overflow is malformed and rejected by
    // check_well_formed; this precedence makes the exploit harmless even so.)
    if let Some(o) = &r.overflow
        && o.armed
        && o.deliveries >= 1
        && stage != Stage::Aa6
    {
        return o.landed_digest.trim();
    }
    // A NON-armed stepped record (AA-2) compares the step-moment state; the final digest can
    // converge, so it cannot establish step identity.
    if let Some(s) = &r.step {
        return s.step_digest.trim();
    }
    r.state_digest.trim()
}

/// Whether a record is **acceptance-bearing** for AA-2 or AA-3: the record whose replay
/// identity IS the stage's acceptance. At AA-2 that is a stepped record (the step-moment
/// state); at AA-3 an armed, delivered landing (the landed state). A record that is neither —
/// a counting run at AA-3, an unarmed sample — is not what the stage certifies, so its being
/// a singleton is not a gap.
///
/// AA-5 is NOT handled here: its acceptance needs BOTH the clock-page and the Linux-guest
/// class present and repeated, a per-CLASS requirement [`check_replay_identity`] enforces in
/// a dedicated branch. AA-6 rests on replay identity too but over the ordinary state digest
/// of every record, covered by the per-group divergence check, not this per-class rule.
fn is_acceptance_bearing(stage: Stage, r: &RunRecord) -> bool {
    match stage {
        Stage::Aa2 => r.step.is_some(),
        // AA-3's landed state: any armed, delivered landing.
        Stage::Aa3 => r
            .overflow
            .as_ref()
            .is_some_and(|o| o.armed && o.deliveries >= 1),
        // AA-4's acceptance is the LSE payload replayed under the same injection schedule — so
        // an armed landing of a NON-LSE payload (straight-line, etc.) is NOT acceptance-bearing
        // here. This is what stops repeated straight-line landings from certifying the atomics
        // contract; the rest of AA-4's evidence is graded by check_aa4_contract.
        Stage::Aa4 => {
            r.payload == Payload::LseAtomics
                && r.overflow
                    .as_ref()
                    .is_some_and(|o| o.armed && o.deliveries >= 1)
        }
        _ => false,
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
        .filter(|r| comparison_digest(stage, r).is_empty())
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
            .entry(comparison_digest(stage, r).to_string())
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

    // A divergent repeated group is the hard failure, whatever the stage — surface it first.
    if !problems.is_empty() {
        out.push(fail(CheckId::ReplayIdentity, join_problems(&problems)));
        return;
    }

    // AA-5 acceptance is TWO bit-identical mechanisms: a work-derived clock-page run AND a
    // Linux-guest boot, each replayed same-seed. BOTH classes must be present AND repeated — a
    // run of only repeated clock-page records has not established the guest boot AA-5(c)
    // requires, and repeated guest boots alone say nothing about the paravirt clock page. The
    // generic acceptance-bearing branch below would PASS on either class alone (its groups are
    // pooled across classes), so AA-5 gets its own per-CLASS branch.
    if stage == Stage::Aa5 {
        let class_groups = |want: Payload| -> Vec<Vec<u64>> {
            let mut g: BTreeMap<RepKey, Vec<u64>> = BTreeMap::new();
            for r in records.iter().filter(|r| r.payload == want) {
                g.entry(rep_key(r)).or_default().push(r.sample_id);
            }
            g.into_values().collect()
        };
        let mut missing: Vec<&str> = Vec::new();
        let mut unrepeated: Vec<&str> = Vec::new();
        let mut singletons: Vec<u64> = Vec::new();
        for (want, label) in [
            (Payload::ClockPage, "clock-page"),
            (Payload::LinuxGuest, "linux-guest"),
        ] {
            let groups = class_groups(want);
            if groups.is_empty() {
                missing.push(label);
            } else if groups.iter().all(|ids| ids.len() < 2) {
                unrepeated.push(label);
            }
            singletons.extend(
                groups
                    .iter()
                    .filter(|ids| ids.len() < 2)
                    .flat_map(|ids| ids.iter().copied()),
            );
        }
        singletons.sort_unstable();

        if !missing.is_empty() {
            out.push(not_requested(
                CheckId::ReplayIdentity,
                format!(
                    "AA-5 acceptance is a bit-identical clock-page run AND a bit-identical \
                     Linux-guest boot, but the records carry no {} record: that half of AA-5 was \
                     never exercised, so this verdict cannot accept it",
                    missing.join(" and no ")
                ),
            ));
        } else if !unrepeated.is_empty() {
            out.push(not_requested(
                CheckId::ReplayIdentity,
                format!(
                    "AA-5's {} class(es) carry no repeated (payload, scale, seed, condition) group \
                     (--reps 1): that state was never replayed, so replay identity is untested. \
                     Submit repeated inputs",
                    unrepeated.join(" and ")
                ),
            ));
        } else if !singletons.is_empty() {
            out.push(fail(
                CheckId::ReplayIdentity,
                format!(
                    "AA-5: samples {} appear once and were never replayed — EVERY clock-page and \
                     Linux-guest group must be repeated (≥2 reps), not just some",
                    preview(singletons.iter().copied())
                ),
            ));
        } else {
            out.push(pass(
                CheckId::ReplayIdentity,
                "both AA-5 classes (clock-page and linux-guest) replayed on bit-identical state \
                 digests"
                    .to_string(),
            ));
        }
        return;
    }

    // AA-2/AA-3/AA-4 rest on the ACCEPTANCE-BEARING groups alone (stepped states / armed
    // landings / LSE-only landings), because the existential `compared > 0` above is not
    // enough: one stepped record per transition (each a singleton group) beside two duplicate
    // UNSTEPPED records makes `compared == 1` and would pass, though not one stepped state was
    // ever replayed; likewise a unique armed AA-3/AA-4 landing. So a repeated unrelated group
    // cannot stand in for the ones the stage certifies.
    if matches!(stage, Stage::Aa2 | Stage::Aa3 | Stage::Aa4) {
        let what = match stage {
            Stage::Aa2 => "stepped",
            Stage::Aa4 => "lse-only landing",
            _ => "armed-landing",
        };
        let mut bearing_groups: BTreeMap<RepKey, Vec<u64>> = BTreeMap::new();
        for r in records.iter().filter(|r| is_acceptance_bearing(stage, r)) {
            bearing_groups
                .entry(rep_key(r))
                .or_default()
                .push(r.sample_id);
        }
        let repeated = bearing_groups.values().filter(|ids| ids.len() >= 2).count();
        let mut singletons: Vec<u64> = bearing_groups
            .values()
            .filter(|ids| ids.len() < 2)
            .flat_map(|ids| ids.iter().copied())
            .collect();
        singletons.sort_unstable();

        if bearing_groups.is_empty() {
            out.push(not_requested(
                CheckId::ReplayIdentity,
                format!(
                    "stage {stage:?} rests on replay-identical {what} state, but no record carries \
                     one — there is nothing to replay. Submit {what} repetitions (--reps); this \
                     verdict cannot accept replay identity it never tested"
                ),
            ));
        } else if repeated == 0 {
            out.push(not_requested(
                CheckId::ReplayIdentity,
                format!(
                    "stage {stage:?} carries {} {what} record(s) but not one is repeated \
                     (--reps 1): no {what} state was replayed, so replay identity is untested — \
                     and an unrelated repeated group does not stand in for a {what} one. Submit \
                     repeated inputs",
                    singletons.len()
                ),
            ));
        } else if !singletons.is_empty() {
            out.push(fail(
                CheckId::ReplayIdentity,
                format!(
                    "stage {stage:?}: {repeated} {what} (payload, scale, seed, condition, target) \
                     group(s) were replayed, but samples {} appear once and were never replayed — \
                     EVERY acceptance-bearing group must be repeated (≥2 reps), not just some",
                    preview(singletons.iter().copied())
                ),
            ));
        } else {
            out.push(pass(
                CheckId::ReplayIdentity,
                format!("{repeated} {what} group(s) each replayed on bit-identical state digests"),
            ));
        }
        return;
    }

    // AA-6 (replay identity over every record's ordinary digest) and the non-replay stages.
    // AA-6 may not PASS having compared NOTHING (with --reps 1 there is no repeated group),
    // so it reads NOT-REQUESTED until the operator submits repeated inputs; the per-group
    // divergence check above already graded any repeated group it does carry.
    if compared == 0 && requires_replay_identity(stage) {
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

/// Whether a stage's acceptance **is** replay identity: AA-2 (replay-identical *stepped*
/// state), AA-3 (replay-identical landed state), AA-4 (the LSE-only payload replayed under the
/// same injection schedule with bit-identical counts and digests), AA-5 (bit-identical
/// same-seed clock-page and Linux-guest runs) and AA-6 (≥1,000 same-input bit-identical). At
/// those, comparing zero digests is not a pass, so a run with no repeated group reads
/// NOT-REQUESTED. Only AA-1 does not rest on it.
const fn requires_replay_identity(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Aa2 | Stage::Aa3 | Stage::Aa4 | Stage::Aa5 | Stage::Aa6
    )
}

/// AA-2 exists to *characterize single-stepping* — does one step retire exactly one
/// instruction, and with what `BR_RETIRED` weight. Its evidence is the **measured**
/// [`StepRecord`](arm_harness::evidence::StepRecord) (PC before/after, instructions retired), not the `exit_reason: debug`
/// enum label, which a rehashed run-set can flip in a single byte. An ordinary
/// `--stage aa2` run ends at the console sentinel with no step record, so without this
/// check the floor reports PASS having observed not one step — the same vacuity class
/// as replay-identity on zero comparisons.
///
/// So this requires the structured evidence and validates it: a step must advance the
/// PC (`pc_after != pc_before`) and retire exactly one instruction (`insn_retired == 1`).
/// A malformed step FAILS; a run carrying no step record at all is NOT-REQUESTED, never
/// PASS. The single-step *run path* is arrival-day (the run loop refuses an unrequested
/// `Debug` exit, and the stepping loop would presume AA-2's own single-step result —
/// the AA-1/AA-2 unknowns the pre-build ruling forbids inventing; the accepted r5
/// skid-landing rebuttal). So today no run emits a `StepRecord` and AA-2 reads
/// NOT-REQUESTED — honestly unexercised — flipping to a real verdict on arrival day.
fn check_debug_evidence(stage: Stage, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if stage != Stage::Aa2 {
        return;
    }
    let stepped: Vec<&RunRecord> = records.iter().filter(|r| r.step.is_some()).collect();

    // The "vice versa": a `Debug` exit with NO step measurement is a bare exit label, not AA-2
    // evidence. A synthetic matrix that flipped `exit_reason` to `debug` without producing a
    // real single step is caught here, and this must fire even when there are no valid stepped
    // records at all — so it is computed before the not-requested early return.
    let mut debug_without_step: Vec<u64> = records
        .iter()
        .filter(|r| r.exit_reason == ExitReason::Debug && r.step.is_none())
        .map(|r| r.sample_id)
        .collect();
    debug_without_step.sort_unstable();

    if stepped.is_empty() && debug_without_step.is_empty() {
        out.push(not_requested(
            CheckId::DebugEvidence,
            "AA-2 certifies single-stepping, and its evidence is the measured step (PC \
             before/after, one instruction retired) — not the exit_reason label. No record \
             carries a step measurement, so not a single step was validated. The stepping \
             run path is arrival-day (the run loop refuses an unrequested debug exit, and \
             the stepping loop would presume AA-2's own single-step result); this verdict \
             cannot accept AA-2 until stepped records exist. NOT a pass.",
        ));
        return;
    }

    let mut bad: Vec<String> = Vec::new();
    for id in &debug_without_step {
        bad.push(format!(
            "sample {id}: exit_reason is Debug but the record carries no step measurement — a \
             debug exit without a validated single step is not AA-2 evidence"
        ));
    }
    let mut covered: BTreeSet<StepTransition> = BTreeSet::new();
    for r in &stepped {
        // `stepped` only holds records whose `step` is Some.
        let s = r.step.as_ref().expect("filtered to Some");
        covered.insert(s.transition);
        // A real single step lands on KVM_EXIT_DEBUG. A step record carrying any other exit
        // was not produced by KVM_GUESTDBG_SINGLESTEP — binding the (byte-flippable)
        // exit_reason label to the measured step is what stops a `mmio`-labelled step matrix.
        if r.exit_reason != ExitReason::Debug {
            bad.push(format!(
                "sample {}: carries a step measurement but exit_reason is {:?}, not Debug — a \
                 single step lands on KVM_EXIT_DEBUG",
                r.sample_id, r.exit_reason
            ));
        }
        if s.pc_after == s.pc_before {
            bad.push(format!(
                "sample {}: step did not advance the PC (pc_before == pc_after == {:#x})",
                r.sample_id, s.pc_before
            ));
        }
        if s.insn_retired != 1 {
            bad.push(format!(
                "sample {}: step retired {} instructions, not the exactly 1 AA-2's single-step \
                 semantics require",
                r.sample_id, s.insn_retired
            ));
        }
        // Validate BR_RETIRED against the transition CLASS the harness recorded from the
        // stepped opcode — NOT from PC arithmetic. `pc_after != pc_before + 4` does not
        // imply a retired branch: an SVC, an abort, an injected IRQ, or an `ERET` all move
        // the PC without a branch INSTRUCTION retiring, and BR_RETIRED counts the branch,
        // not the transfer. So `delta == 1` is forced only where the architecture
        // guarantees a retired branch; the exception/WFI/injection classes are where AA-2
        // *measures* the weight (e.g. ERET's is unknown by construction), bounded only by
        // "a single step retires at most one taken branch".
        match s.transition {
            StepTransition::Sequential => {
                if s.br_retired_delta != 0 {
                    bad.push(format!(
                        "sample {}: a sequential step must not move BR_RETIRED, but delta is {}",
                        r.sample_id, s.br_retired_delta
                    ));
                }
                // AArch64 instructions are a fixed 4 bytes, so a sequential (fall-through)
                // step lands at EXACTLY PC+4. A larger advance means an instruction was
                // skipped, and an equal-or-smaller one that it doubled or stalled — both are
                // exactly the miss-count AA-2 exists to detect, so require the exact next
                // address, not merely a different PC.
                if s.pc_after != s.pc_before.wrapping_add(4) {
                    bad.push(format!(
                        "sample {}: a sequential step must land at PC+4 ({:#x}), but landed at \
                         {:#x} — a skipped or doubled instruction, which AA-2 must catch",
                        r.sample_id,
                        s.pc_before.wrapping_add(4),
                        s.pc_after
                    ));
                }
            }
            StepTransition::TakenBranch if s.br_retired_delta != 1 => bad.push(format!(
                "sample {}: a taken branch must increment BR_RETIRED by exactly 1, but delta is {}",
                r.sample_id, s.br_retired_delta
            )),
            // A single-stepped LL/SC exclusive (`LDXR`/`STXR`) is a load or a store, not a
            // taken branch, so BR_RETIRED must NOT move. (The retry is a separate `CBNZ`,
            // stepped and classified as a TakenBranch in its own right.) Grouping it with
            // the exception/WFI/injection classes wrongly admitted a delta of 1.
            StepTransition::LlscExclusive if s.br_retired_delta != 0 => bad.push(format!(
                "sample {}: a single-stepped LL/SC exclusive (LDXR/STXR) is not a taken branch, \
                 so BR_RETIRED must not move, but delta is {}",
                r.sample_id, s.br_retired_delta
            )),
            StepTransition::ExceptionEntry
            | StepTransition::ExceptionReturn
            | StepTransition::Wfi
            | StepTransition::Injection
                if s.br_retired_delta > 1 =>
            {
                bad.push(format!(
                    "sample {}: a single {:?} step cannot retire more than one BR_RETIRED, but \
                     delta is {}",
                    r.sample_id, s.transition, s.br_retired_delta
                ));
            }
            _ => {}
        }
    }

    // COVERAGE. AA-2 characterizes single-stepping ACROSS THE MATRIX — every transition
    // class, not one. One valid sequential step beside seven `step: null` records is not
    // AA-2; a partial set that graded clean would be exactly the "green on absent
    // evidence" vacuity. So the required transitions must ALL be covered.
    let missing: Vec<StepTransition> = REQUIRED_AA2_TRANSITIONS
        .iter()
        .copied()
        .filter(|t| !covered.contains(t))
        .collect();
    if !missing.is_empty() {
        bad.push(format!(
            "the AA-2 step matrix is incomplete: no step covers {missing:?} — the stage requires \
             stepping every transition class (sequential, taken branch, exception entry, ERET, \
             WFI, injection), not merely a nonempty subset"
        ));
    }

    if bad.is_empty() {
        out.push(pass(
            CheckId::DebugEvidence,
            format!(
                "{} record(s) cover the full AA-2 step matrix, each a valid single step with a \
                 BR_RETIRED delta consistent with its transition class",
                stepped.len()
            ),
        ));
    } else {
        out.push(fail(
            CheckId::DebugEvidence,
            format!(
                "{} AA-2 step problem(s): {}",
                bad.len(),
                join_problems(&bad)
            ),
        ));
    }
}

/// The transition classes an AA-2 run must step across — the coverage matrix. Fewer than
/// all of these is a partial characterization, not AA-2's. The LL/SC exclusive is
/// explicit: AA-2 must step an `LDXR`/`STXR` sequence to characterize the
/// monitor-clearing/livelock behaviour AA-4's LSE-only contract rests on, and a run that
/// stepped every OTHER class but no exclusive has not measured it.
const REQUIRED_AA2_TRANSITIONS: &[StepTransition] = &[
    StepTransition::Sequential,
    StepTransition::TakenBranch,
    StepTransition::ExceptionEntry,
    StepTransition::ExceptionReturn,
    StepTransition::Wfi,
    StepTransition::Injection,
    StepTransition::LlscExclusive,
];

/// The classes AA-6's determinism matrix must cover: every payload with a counting window
/// PLUS the AA-5 Linux guest ([`Payload::LinuxGuest`]). The binding AA-6 matrix is "the
/// payload matrix plus the AA-5 Linux guest" (`docs/ARM-ALTRA.md` §AA-6), so a run of the
/// eight bare-metal payloads alone — however many reps — is not AA-6. The `ident`
/// capability report has no measured count and is not part of the rep matrix.
fn required_aa6_classes() -> Vec<Payload> {
    let mut classes: Vec<Payload> = ALL_PAYLOADS
        .iter()
        .copied()
        .filter(|p| p.has_window())
        .collect();
    classes.push(Payload::LinuxGuest);
    classes
}

/// AA-6's mini determinism gate is over a **matrix** of classes, not one input. The
/// rep floor ([`check_floors`]) only grades inputs that are *present*, so 1,000 copies
/// of a single `straight-line` record satisfies `--min-reps 1000` while every other
/// required class is silently absent. This verifies the matrix is complete *before*
/// the rep floor's per-group count means anything.
///
/// The matrix includes the **AA-5 Linux guest** ([`Payload::LinuxGuest`]): no run produces
/// one pre-silicon, so requiring it keeps AA-6 honestly unfulfilled until arrival day
/// rather than letting 1,000 reps of the eight bare-metal payloads report a passing AA-6.
fn check_aa6_matrix(stage: Stage, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if stage != Stage::Aa6 {
        return;
    }
    // Coverage is from ARMED, DELIVERED records — the ones that actually INJECTED an event.
    // AA-6 certifies determinism UNDER injection; a class present only as UNARMED records
    // injected nothing, so counting payload labels alone let a run supply repeated unarmed
    // records for the required classes, one armed class, `--min-reps`/`--min-armed-overflows
    // 1`, and pass without injecting across the matrix. Arming across the matrix is AA-6's
    // invariant, enforced here rather than by an (undefined) numeric armed floor.
    let injected: BTreeSet<Payload> = records
        .iter()
        .filter(|r| {
            r.overflow
                .as_ref()
                .is_some_and(|o| o.armed && o.deliveries >= 1)
        })
        .map(|r| r.payload)
        .collect();
    let required = required_aa6_classes();
    let missing: Vec<Payload> = required
        .iter()
        .copied()
        .filter(|p| !injected.contains(p))
        .collect();
    if missing.is_empty() {
        out.push(pass(
            CheckId::Aa6Matrix,
            format!(
                "all {} required classes have an injected (armed, delivered) record in the AA-6 \
                 determinism matrix (payloads + the AA-5 Linux guest)",
                required.len()
            ),
        ));
    } else {
        let names: Vec<&str> = missing.iter().map(|p| p.name()).collect();
        out.push(fail(
            CheckId::Aa6Matrix,
            format!(
                "AA-6's determinism matrix is incomplete: {} class(es) have no injected (armed, \
                 delivered) record — {}. AA-6 certifies determinism UNDER injection, so a class \
                 present only as unarmed records injected nothing; the matrix must be verified \
                 from what was actually injected, not from payload labels.",
                missing.len(),
                names.join(", ")
            ),
        ));
    }
}

/// AA-4 is the **LSE-only atomics contract** (`docs/ARM-ALTRA.md` §4). Its acceptance is a
/// STRUCTURED set of experiments, none of which a repeated non-LSE landing satisfies:
///   1. the LSE payload's count-invariance under injection — an armed `lse-atomics` landing,
///      replayed on the same schedule (enforced as the acceptance-bearing record in
///      [`check_replay_identity`], so a straight-line landing is not one);
///   2. the LL/SC DIVERGENCE demonstration that motivates the ban — `llsc-atomics` shown
///      non-deterministic under injection;
///   3. the THREE enforcement levels — build-level LSE-only kernel+userspace, the boot-time
///      exclusive scan, and trap-and-emulate of residual exclusives;
///   4. the recorded LL/SC RULING — mechanically-unreachable vs cooperative-residual.
///
/// Experiments (2)–(4) are ARRIVAL-DAY: the guest kernel build, the exclusive scan, the
/// `HCR_EL2`/`MDCR_EL2` trap tables, and the ruling are not representable in the current record
/// schema (no run produces them pre-silicon). So AA-4 reads NOT-REQUESTED — honestly
/// unexercised — never PASS: repeated non-LSE landings must not certify the atomics contract.
fn check_aa4_contract(stage: Stage, records: &[RunRecord], out: &mut Vec<Outcome>) {
    if stage != Stage::Aa4 {
        return;
    }
    let armed_lse = records.iter().any(|r| {
        r.payload == Payload::LseAtomics
            && r.overflow
                .as_ref()
                .is_some_and(|o| o.armed && o.deliveries >= 1)
    });
    let llsc_present = records.iter().any(|r| r.payload == Payload::LlscAtomics);
    out.push(not_requested(
        CheckId::Aa4Contract,
        format!(
            "AA-4 (the LSE-only atomics contract, docs/ARM-ALTRA.md §4) rests on a structured \
             evidence set: an armed lse-atomics landing replayed under injection [{}], the LL/SC \
             divergence demonstration [{}], the three enforcement levels (build-level LSE-only \
             kernel+userspace, boot-time exclusive scan, trap-and-emulate of residual \
             exclusives), and the recorded LL/SC ruling (mechanically-unreachable vs \
             cooperative-residual). The enforcement levels and the ruling are arrival-day \
             deliverables not representable in the record schema, so this verdict cannot accept \
             AA-4 — a straight-line landing must not certify the atomics contract.",
            if armed_lse { "present" } else { "ABSENT" },
            if llsc_present {
                "llsc-atomics records present"
            } else {
                "no llsc-atomics records"
            },
        ),
    ));
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
/// The normative (binding) acceptance floors, `docs/ARM-ALTRA.md`: AA-1/AA-3 land
/// ≥1,000,000 armed overflows; AA-6's mini gate repeats each input ≥1,000 times. A
/// floor below these does not certify the stage — it must fail closed unless the
/// operator opts into a `SUB-NORMATIVE` verdict explicitly.
const NORMATIVE_ARMED_FLOOR: u64 = 1_000_000;
const NORMATIVE_REP_FLOOR: u64 = 1_000;

/// The stage's normative armed-overflow minimum, if it has one. The binding document
/// defines the ≥1,000,000 floor for **AA-1 and AA-3 only**; AA-4 (the LSE-only contract)
/// rides AA-3's machinery but is not itself held to the million-overflow floor, so
/// including it would reject contract-valid AA-4 evidence unless it were mislabelled
/// SUB-NORMATIVE.
const fn normative_armed_floor(stage: Stage) -> Option<u64> {
    match stage {
        Stage::Aa1 | Stage::Aa3 => Some(NORMATIVE_ARMED_FLOOR),
        _ => None,
    }
}

/// The stage's normative repetition minimum, if it has one.
const fn normative_rep_floor(stage: Stage) -> Option<u64> {
    match stage {
        Stage::Aa6 => Some(NORMATIVE_REP_FLOOR),
        _ => None,
    }
}

/// Count the armed overflows in a record set.
fn count_armed(records: &[RunRecord]) -> u64 {
    records
        .iter()
        .filter(|r| r.overflow.as_ref().is_some_and(|o| o.armed))
        .count() as u64
}

/// Distinct armed CASES across a record set — one per `(payload, scale, seed, condition,
/// target_delta)` armed, delivered input. Reps of a case share its [`RepKey`], so this counts
/// inputs, not deadlines. The target delta (not the absolute target) is used, matching
/// [`rep_key`], so reps whose pre-window execution shifted `work_begin` still count as one case.
fn distinct_armed_cases(records: &[RunRecord]) -> BTreeSet<CaseKey> {
    records
        .iter()
        .filter(|r| {
            r.overflow
                .as_ref()
                .is_some_and(|o| o.armed && o.deliveries >= 1)
        })
        .map(case_key)
        .collect()
}

/// The key a distinct armed CASE is counted under — `(payload, scale, seed, target_delta)`,
/// deliberately OMITTING `condition`. A case is a distinct seeded-random input drawn by the
/// plan; the contamination `condition` is the sweep axis run OVER each case, not part of the
/// case's identity. Keying by `rep_key` (which includes `condition`) let four same-seed
/// condition run-sets — which draw IDENTICAL cases — count as four distinct cases and satisfy
/// `--min-cases 4` with a single target. The delta (not the absolute target) matches `rep_key`.
type CaseKey = (String, String, u64, Option<i128>);

fn case_key(r: &RunRecord) -> CaseKey {
    (
        r.payload.name().to_string(),
        r.scale.name().to_string(),
        r.seed,
        r.overflow
            .as_ref()
            .map(|o| i128::from(o.target) - i128::from(r.work_begin)),
    )
}

/// The `--min-cases` floor, over an ALREADY-COLLECTED distinct-case count. AA-1/AA-3 rest on a
/// DISTRIBUTION of seeded-random targets, not one target cloned across reps to the deadline
/// total: `--cases 1 --reps N` meets the ≥10⁶ armed floor from a handful of cases while the
/// target/skid distribution is barely exercised, and the rep/count checks — which count every
/// repetition — do not catch it. So distinct-case coverage is enforced SEPARATELY here.
///
/// Fires only where a stage defines an armed floor (AA-1/AA-3) and only when deadlines were
/// actually armed (a counting-mode AA-1(b) run has no target distribution — the armed floor
/// reports its absence). An armed run with no `--min-cases` reads NOT-REQUESTED — the
/// distinct-case coverage was never bounded — never a silent pass.
fn check_case_coverage(stage: Stage, floors: &Floors, armed_cases: u64, out: &mut Vec<Outcome>) {
    if normative_armed_floor(stage).is_none() || armed_cases == 0 {
        return;
    }
    match floors.min_cases {
        // A floor of zero is vacuous: every run has "at least 0" distinct cases.
        Some(0) => out.push(fail(
            CheckId::CaseCoverage,
            "a --min-cases floor of 0 certifies nothing: it is met by a run that cloned a single \
             target across every rep. Pass a nonzero floor — AA-1/AA-3 rest on a DISTRIBUTION of \
             seeded-random targets, not one target repeated to the deadline total."
                .to_string(),
        )),
        Some(min) => {
            if armed_cases >= min {
                out.push(pass(
                    CheckId::CaseCoverage,
                    format!(
                        "{armed_cases} distinct armed target/seed case(s) meets the --min-cases \
                         floor of {min}"
                    ),
                ));
            } else {
                out.push(fail(
                    CheckId::CaseCoverage,
                    format!(
                        "only {armed_cases} distinct armed target/seed case(s), below the \
                         --min-cases floor of {min}: the armed floor was met by cloning a handful \
                         of cases across reps, so the seeded-random target/skid distribution \
                         AA-1/AA-3 rest on was barely exercised. The `cases` dimension must bind, \
                         not just `reps`."
                    ),
                ));
            }
        }
        None => out.push(not_requested(
            CheckId::CaseCoverage,
            format!(
                "{armed_cases} distinct armed target/seed case(s) present, but no --min-cases \
                 floor was requested: the ≥10⁶ armed floor can be met by cloning a few cases \
                 across many reps, so the distinct-case coverage AA-1/AA-3 rest on must be bound \
                 separately from the deadline count. This verdict cannot accept it un-bounded."
            ),
        )),
    }
}

/// The armed-overflow floor, over an ALREADY-SUMMED armed count. Split out of
/// [`check_floors`] so a condition-matrix run — one run-set per contamination condition
/// — checks the CUMULATIVE 1,000,000 floor over the union of all conditions, which is
/// how AA-1's floor is defined, rather than demanding each condition reach it alone.
fn check_armed_floor(stage: Stage, floors: &Floors, armed: u64, out: &mut Vec<Outcome>) {
    // A stage whose acceptance IS armed deadlines (AA-3's ≥10⁶ armed overflows,
    // landed exactly) may not pass without armed records OR without the floor being
    // requested. The missing-floor case must NOT be gated on `armed > 0`: an AA-3 run
    // submitted with no armed records (e.g. run without `--with-targets`) would then
    // emit no floor outcome at all, and the mechanism and skid checks have nothing to
    // inspect — so AA-3 would pass without testing a single deadline. The requirement
    // is enforced on the STAGE, independent of what the records happened to contain.
    // A stage requires an armed-overflow FLOOR only if it DEFINES one: AA-1 (skid/count
    // distribution) and AA-3 (exact landings) both land ≥1000000 armed overflows. AA-4 and
    // AA-6 ride the patched mechanism but define NO armed-overflow floor — AA-6's floor is
    // ≥1000 repetitions and its arming is a MATRIX invariant (every class must have an
    // injected record, `check_aa6_matrix`), NOT a numeric floor. Requiring `--min-armed-
    // overflows` there made valid AA-6 evidence read INCOMPLETE unless the operator invented
    // an undocumented number. (A counting-only AA-1(b) run still reads NOT-REQUESTED here — a
    // distinguished sub-experiment verdict, never a silent stage PASS.)
    let requires_armed = normative_armed_floor(stage).is_some();
    match floors.min_armed_overflows {
        // A floor of zero is not a floor: `armed >= 0` holds for a run that armed no
        // deadline at all, so `--min-armed-overflows 0` is exactly the vacuous pass the
        // floor exists to prevent. Reject it outright.
        Some(0) => out.push(fail(
            CheckId::ArmedOverflowFloor,
            "a --min-armed-overflows floor of 0 certifies nothing: it is met by a run that \
             armed no deadline. Pass a nonzero floor (the AA-1/AA-3 acceptance floor is \
             1000000 cumulative)."
                .to_string(),
        )),
        Some(min) => {
            let normative = normative_armed_floor(stage);
            let below = normative.is_some_and(|norm| min < norm);
            if below && !floors.sub_normative {
                // A below-normative floor may not silently produce an acceptance.
                out.push(fail(
                    CheckId::ArmedOverflowFloor,
                    format!(
                        "the requested floor {min} is below the stage-normative minimum {} \
                         (AA-1/AA-3 land 1000000 armed overflows). Pass --sub-normative to accept \
                         a weakened verdict — it will be marked SUB-NORMATIVE, never silent.",
                        normative.unwrap_or(NORMATIVE_ARMED_FLOOR)
                    ),
                ));
            } else {
                // A weakened but explicitly-permitted floor is tagged so the verdict can
                // never be mistaken for a normative acceptance.
                let tag = if below { " [SUB-NORMATIVE]" } else { "" };
                if armed >= min {
                    out.push(pass(
                        CheckId::ArmedOverflowFloor,
                        format!("{armed} armed overflows meets the floor of {min}{tag}"),
                    ));
                } else {
                    out.push(fail(
                        CheckId::ArmedOverflowFloor,
                        format!("only {armed} armed overflows, below the floor of {min}{tag}"),
                    ));
                }
            }
        }
        None if requires_armed => out.push(not_requested(
            CheckId::ArmedOverflowFloor,
            format!(
                "stage {stage:?} is certified BY its armed-overflow floor (AA-1's skid/count \
                 distribution and AA-3's exact landings both rest on ≥1000000 cumulative armed \
                 overflows), but no --min-armed-overflows floor was requested and the records \
                 carry {armed} armed overflow(s). This is a counting SUB-EXPERIMENT, not a \
                 stage-level acceptance — pass the floor explicitly to certify the stage."
            ),
        )),
        // No floor requested for a stage that does not DEFINE an armed-overflow floor (AA-2,
        // AA-4, AA-5, AA-6). Armed records here are legitimate and their determinism is
        // enforced by the stage's own invariant — AA-6's matrix (`check_aa6_matrix` requires
        // an injected record per class), AA-4's LSE contract, AA-5's replay identity — not by
        // a numeric armed floor. Emit no floor outcome: requiring one made valid AA-6 evidence
        // read INCOMPLETE against an undefined number.
        None => {}
    }
}

/// Both floors together, over one run-set — the single-set convenience the unit tests
/// drive. Production paths call [`check_armed_floor`] and [`check_rep_floor`] directly
/// (so the armed floor can be cumulative across a condition matrix).
#[cfg(test)]
fn check_floors(run_set: &RunSet, floors: &Floors, records: &[RunRecord], out: &mut Vec<Outcome>) {
    check_armed_floor(run_set.stage, floors, count_armed(records), out);
    check_rep_floor(run_set, floors, records, out);
}

fn check_rep_floor(
    run_set: &RunSet,
    floors: &Floors,
    records: &[RunRecord],
    out: &mut Vec<Outcome>,
) {
    // The rep floor is PER-REPEATED-INPUT, not total rows. AA-6 needs ≥1000
    // repetitions of the SAME (payload, scale, seed, condition, target) input,
    // bit-identical. Counting total records would let 1,000 rows that are 125 reps of
    // an eight-payload matrix pass a 1,000 floor, though no single input was repeated
    // 1,000 times — which is not the same-seed determinism the gate certifies. So the
    // floor is the count of the *least-repeated* distinct input: every group must meet
    // it. (replay-identity then checks those reps actually landed identically.)
    match floors.min_reps {
        // A rep floor of zero is vacuous the same way: every input trivially has "at
        // least 0" repetitions. Reject it.
        Some(0) => out.push(fail(
            CheckId::RepFloor,
            "a --min-reps floor of 0 certifies nothing: every input meets it. Pass a nonzero \
             floor (AA-6's mini gate is 1000 same-input repetitions)."
                .to_string(),
        )),
        Some(min) => {
            let normative = normative_rep_floor(run_set.stage);
            let below = normative.is_some_and(|norm| min < norm);
            if below && !floors.sub_normative {
                out.push(fail(
                    CheckId::RepFloor,
                    format!(
                        "the requested rep floor {min} is below AA-6's normative minimum {} \
                         same-input repetitions. Pass --sub-normative to accept a weakened verdict \
                         — it will be marked SUB-NORMATIVE, never silent.",
                        normative.unwrap_or(NORMATIVE_REP_FLOOR)
                    ),
                ));
            } else {
                let tag = if below { " [SUB-NORMATIVE]" } else { "" };
                // AA-6's floor is over repetitions UNDER INJECTION. A group of one armed
                // record and 999 `armed: false` records sharing the same target is not 1,000
                // injected reps: check_replay_identity compares their (final) digests, but the
                // rep FLOOR must not read 1,000 there. So at AA-6 only armed, delivered records
                // count toward the floor; other stages that opt into a floor count every record
                // (their acceptance is not injection).
                let counted: Vec<&RunRecord> = records
                    .iter()
                    .filter(|r| {
                        run_set.stage != Stage::Aa6
                            || r.overflow
                                .as_ref()
                                .is_some_and(|o| o.armed && o.deliveries >= 1)
                    })
                    .collect();
                let mut groups: BTreeMap<RepKey, u64> = BTreeMap::new();
                for r in &counted {
                    *groups.entry(rep_key(r)).or_default() += 1;
                }
                let distinct = groups.len();
                let min_group = groups.values().copied().min().unwrap_or(0);
                if min_group >= min {
                    out.push(pass(
                        CheckId::RepFloor,
                        format!(
                            "{distinct} distinct input(s), each repeated at least {min_group} times \
                             (floor {min}){tag}"
                        ),
                    ));
                } else {
                    out.push(fail(
                        CheckId::RepFloor,
                        format!(
                            "the least-repeated input appears only {min_group} time(s), below the \
                             per-input rep floor of {min} (there are {distinct} distinct input(s) \
                             across {} counted record(s); a total-count floor — or counting unarmed \
                             records at AA-6 — would have hidden this: AA-6 needs {min} reps of the \
                             SAME input UNDER injection){tag}",
                            counted.len()
                        ),
                    ));
                }
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
    use arm_harness::evidence::StepRecord;
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
            step: None,
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

    fn detail(out: &[Outcome], id: CheckId) -> String {
        out.iter()
            .find(|o| o.id == id)
            .map(|o| o.detail.clone())
            .unwrap_or_default()
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
        // AA-1's bounded probe: legitimate — unpinned AND no pinned core, so it really
        // does migrate (the rr #3607 failure mode it exists to exercise).
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        rs.pinning.pinned = false;
        rs.pinning.core = None;
        rs.pinning.migration_probe = true;
        let mut out = Vec::new();
        check_pinning(&rs, &mut out);
        assert_eq!(status(&out, CheckId::Pinning), Some(Status::Pass));

        // A "migration probe" that is nonetheless pinned, or carries a pinned core, is
        // contradictory — it never migrates — and is refused even at AA-1.
        for (pinned, core) in [(true, None), (false, Some(2)), (true, Some(2))] {
            let mut rs = a_run_set();
            rs.stage = Stage::Aa1;
            rs.pinning.pinned = pinned;
            rs.pinning.core = core;
            rs.pinning.migration_probe = true;
            let mut out = Vec::new();
            check_pinning(&rs, &mut out);
            assert_eq!(
                status(&out, CheckId::Pinning),
                Some(Status::Fail),
                "a pinned migration probe is contradictory (pinned={pinned}, core={core:?})"
            );
        }

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
    fn a_lost_pmi_sentinel_exit_is_not_a_mechanism_mismatch() {
        // Consistent with r24's multiplicity carve-out: a LOST PMI (armed, deliveries == 0) ends
        // at the console sentinel with ExitReason::Mmio, NOT the mechanism exit. Grading it as an
        // exit-reason mismatch would re-penalise the probe-loss signature the multiplicity check
        // now accepts. The exit-reason comparison is scoped to DELIVERED overflows.
        let rs = a_run_set(); // AA-3, expected_exit_reason = Preempt
        let mut lost = a_record(0);
        if let Some(o) = lost.overflow.as_mut() {
            o.deliveries = 0;
        }
        lost.exit_reason = ExitReason::Mmio;
        let mut out = Vec::new();
        check_mechanism(&rs, &[lost], &mut out);
        assert_ne!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail),
            "a lost PMI ends at the sentinel, not the mechanism exit — not a mismatch"
        );

        // A genuinely DELIVERED record with the wrong exit still fails.
        let mut wrong = a_record(1); // armed, deliveries == 1
        wrong.exit_reason = ExitReason::SignalKick; // not the claimed Preempt
        let mut out = Vec::new();
        check_mechanism(&rs, &[wrong], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail),
            "a delivered record must carry the claimed mechanism exit"
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
    fn aa5_certifies_only_a_work_derived_clock_page() {
        let mut rs = a_run_set();
        rs.stage = Stage::Aa5;

        // A clock-page record: the payload whose mode the attestation is about.
        let clockpage = |mode: Option<&str>| {
            let mut r = a_record(0);
            r.payload = Payload::ClockPage;
            r.clockpage_mode = mode.map(str::to_string);
            r
        };

        // The self-seeded fallback: the payload published its own static page. Hard fail.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(Some("self-seeded"))], &mut out);
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Fail));

        // No attestation at all is not better than the wrong one.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(None)], &mut out);
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Fail));

        // A static managed page: the harness published it (plumbing OK), but it is not
        // the work-derived clock AA-5 certifies. NOT-REQUESTED (silicon-day deferral),
        // never a pass — a static page must not certify AA-5.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(Some("managed-static"))], &mut out);
        assert_eq!(
            status(&out, CheckId::ClockPageMode),
            Some(Status::NotRequested),
            "a static managed page proves plumbing, not the work-derived clock"
        );

        // The work-derived page: the only thing AA-5 accepts.
        let mut out = Vec::new();
        check_clockpage_mode(&rs, &[clockpage(Some("work-derived"))], &mut out);
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Pass));

        // The Linux guest is clock-attesting too: a work-derived clock-page beside a guest
        // that attests NO work-derived time is not AA-5 — presence is not evidence.
        let guest = |mode: Option<&str>| {
            let mut r = a_record(1);
            r.payload = Payload::LinuxGuest;
            r.clockpage_mode = mode.map(str::to_string);
            r
        };
        let mut out = Vec::new();
        check_clockpage_mode(
            &rs,
            &[clockpage(Some("work-derived")), guest(None)],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ClockPageMode),
            Some(Status::Fail),
            "a Linux guest with no clock attestation fails even beside a work-derived clock-page"
        );

        // Both classes work-derived → PASS.
        let mut out = Vec::new();
        check_clockpage_mode(
            &rs,
            &[clockpage(Some("work-derived")), guest(Some("work-derived"))],
            &mut out,
        );
        assert_eq!(status(&out, CheckId::ClockPageMode), Some(Status::Pass));

        // An AA-5 run-set with NO clock-page records at all is a vacuous pass waiting
        // to happen — the mechanism AA-5 certifies was never exercised.
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
        check_clockpage_mode(&rs, &[clockpage(Some("work-derived"))], &mut out);
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
    fn a_below_normative_armed_floor_fails_closed_unless_opted_into() {
        // `--min-armed-overflows 8` at AA-3 is far below the normative 1,000,000. A
        // silent pass on it is exactly the weakened verdict this closes: it FAILS unless
        // --sub-normative is passed, and then the outcome is tagged so it can never read
        // as a normative acceptance.
        let below = Floors {
            min_armed_overflows: Some(8),
            min_reps: None,
            min_cases: None,
            sub_normative: false,
        };
        let mut out = Vec::new();
        check_armed_floor(Stage::Aa3, &below, 16, &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::Fail),
            "a below-normative floor must not silently pass"
        );

        let opted = Floors {
            min_cases: None,
            sub_normative: true,
            ..below
        };
        let mut out = Vec::new();
        check_armed_floor(Stage::Aa3, &opted, 16, &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::Pass)
        );
        assert!(
            detail(&out, CheckId::ArmedOverflowFloor).contains("SUB-NORMATIVE"),
            "a weakened pass must be marked, never indistinguishable from a normative one"
        );

        // The normative floor itself passes clean (no tag), over a CUMULATIVE count —
        // this is what a condition matrix sums to.
        let normative = Floors {
            min_armed_overflows: Some(1_000_000),
            min_reps: None,
            min_cases: None,
            sub_normative: false,
        };
        let mut out = Vec::new();
        check_armed_floor(Stage::Aa3, &normative, 1_000_000, &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::Pass)
        );
        assert!(!detail(&out, CheckId::ArmedOverflowFloor).contains("SUB-NORMATIVE"));
    }

    /// Build an in-memory (manifest, records, bytes) run-set with a chosen stage,
    /// condition, records-hash, and armed-record count — enough to drive `aggregate`.
    fn a_loaded_set(
        stage: Stage,
        id: &str,
        condition: &str,
        hash: &str,
        n: u64,
    ) -> (RunSet, Vec<RunRecord>, Vec<u8>) {
        let mut rs = a_run_set();
        rs.stage = stage;
        rs.run_set_id = id.into();
        rs.condition = condition.into();
        rs.records_sha256 = hash.into();
        let records: Vec<RunRecord> = (0..n)
            .map(|i| {
                let mut r = a_record(i);
                r.condition = condition.into();
                r
            })
            .collect();
        (rs, records, Vec::new())
    }

    #[test]
    fn aggregate_sums_distinct_run_sets_and_rejects_duplicates() {
        let floors = Floors {
            min_armed_overflows: Some(32),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };

        // Two DISTINCT AA-3 run-sets (16 armed each) sum to a cumulative 32.
        let two = [
            a_loaded_set(Stage::Aa3, "solo", "pinned-solo", &"a".repeat(64), 16),
            a_loaded_set(
                Stage::Aa3,
                "cotenant",
                "co-tenant-other-core",
                &"b".repeat(64),
                16,
            ),
        ];
        let out = aggregate(&two, &floors).outcomes;
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::Pass),
            "16 + 16 armed overflows meet a cumulative floor of 32"
        );
        assert_eq!(status(&out, CheckId::Aggregation), Some(Status::Pass));

        // 33 is not met by 32.
        let strict = Floors {
            min_armed_overflows: Some(33),
            ..floors
        };
        assert_eq!(
            status(
                &aggregate(&two, &strict).outcomes,
                CheckId::ArmedOverflowFloor
            ),
            Some(Status::Fail)
        );

        // The SAME set twice (same id and records hash) is a duplicate: the aggregation
        // fails, so 16 doubled cannot masquerade as a cumulative 32.
        let dup = [
            a_loaded_set(Stage::Aa3, "solo", "pinned-solo", &"a".repeat(64), 16),
            a_loaded_set(Stage::Aa3, "solo", "pinned-solo", &"a".repeat(64), 16),
        ];
        assert_eq!(
            status(&aggregate(&dup, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail),
            "the same run-set twice must not sum"
        );
    }

    #[test]
    fn aa1_aggregation_requires_the_full_contamination_matrix() {
        let floors = Floors {
            min_armed_overflows: Some(2),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };

        // Missing same-core and memory-pressure: the matrix is incomplete → FAIL.
        let partial = [
            a_loaded_set(Stage::Aa1, "solo", "pinned-solo", &"a".repeat(64), 2),
            a_loaded_set(
                Stage::Aa1,
                "other",
                "co-tenant-other-core",
                &"b".repeat(64),
                2,
            ),
        ];
        assert_eq!(
            status(
                &aggregate(&partial, &floors).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Fail),
            "a partial contamination matrix must not pass AA-1"
        );

        // The full matrix → PASS.
        let full = [
            a_loaded_set(Stage::Aa1, "c0", "pinned-solo", &"a".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "c1", "co-tenant-other-core", &"b".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "c2", "co-tenant-same-core", &"c".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "c3", "memory-pressure", &"d".repeat(64), 2),
        ];
        assert_eq!(
            status(
                &aggregate(&full, &floors).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Pass),
            "the complete contamination matrix passes"
        );
    }

    #[test]
    fn aa1_matrix_needs_each_condition_measured_not_merely_labelled() {
        // The self-sweep gap: all four condition LABELS are present, but only pinned-solo
        // carries armed overflows — the other three are counting-mode (0 armed) run-sets.
        // The cumulative floor is met by pinned-solo alone, yet count invariance under the
        // three contamination conditions was never measured. Label presence is not enough.
        let unarmed = |id: &str, cond: &str, hash: &str| -> (RunSet, Vec<RunRecord>, Vec<u8>) {
            let mut set = a_loaded_set(Stage::Aa1, id, cond, hash, 2);
            for r in &mut set.1 {
                r.overflow = None; // counting mode: no armed overflow
            }
            set
        };
        let mislabelled = [
            a_loaded_set(Stage::Aa1, "solo", "pinned-solo", &"a".repeat(64), 4),
            unarmed("other", "co-tenant-other-core", &"b".repeat(64)),
            unarmed("same", "co-tenant-same-core", &"c".repeat(64)),
            unarmed("mem", "memory-pressure", &"d".repeat(64)),
        ];
        let floors = Floors {
            min_armed_overflows: Some(4),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };
        assert_eq!(
            status(
                &aggregate(&mislabelled, &floors).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Fail),
            "a condition present in name but with zero armed overflows was never measured"
        );
    }

    #[test]
    fn aggregated_run_sets_must_share_constants_and_environment() {
        let floors = Floors {
            min_armed_overflows: Some(2),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };
        let pair = || {
            [
                a_loaded_set(Stage::Aa1, "a", "pinned-solo", &"a".repeat(64), 2),
                a_loaded_set(Stage::Aa1, "b", "co-tenant-other-core", &"b".repeat(64), 2),
            ]
        };

        // Identical constants pack + environment → the aggregation check itself passes
        // (condition `a`/`b` are the sweep variable and are allowed to differ).
        assert_eq!(
            status(&aggregate(&pair(), &floors).outcomes, CheckId::Aggregation),
            Some(Status::Pass)
        );

        // A different weights (constants) pack → refused: a condition-dependent count change
        // could hide behind the compensating offset.
        let mut w = pair();
        w[1].0.weights = Some(Weights::measured(9, 9, 9, 9, 2));
        assert_eq!(
            status(&aggregate(&w, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail)
        );

        // A different measurement environment (kernel) → refused.
        let mut e = pair();
        e[1].0.environment.host_kernel = "6.18.35-other".into();
        assert_eq!(
            status(&aggregate(&e, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail)
        );

        // A different perf configuration → refused.
        let mut p = pair();
        p[1].0.perf.exclude_hv = false;
        assert_eq!(
            status(&aggregate(&p, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail)
        );

        // A different payload image (binary content hash) → refused: the workload changed
        // between conditions, so any count difference or invariance is an artefact.
        let mut im = pair();
        im[1].0.images[0].sha256 = format!("sha256:{}", "f".repeat(64));
        assert_eq!(
            status(&aggregate(&im, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail)
        );
    }

    #[test]
    fn aggregation_holds_pinning_fixed_except_the_migration_probe() {
        let floors = Floors {
            min_armed_overflows: Some(2),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };
        // Two AA-3 sets that agree on everything the sweep does not vary.
        let pair = || {
            [
                a_loaded_set(Stage::Aa3, "a", "pinned-solo", &"a".repeat(64), 2),
                a_loaded_set(Stage::Aa3, "b", "pinned-solo", &"b".repeat(64), 2),
            ]
        };
        assert_eq!(
            status(&aggregate(&pair(), &floors).outcomes, CheckId::Aggregation),
            Some(Status::Pass),
            "same core + governor → comparable"
        );

        // A different pinned core → refused: the PMU/skid measurement environment moved, so
        // two half-floors are not one floor of one measurement.
        let mut c = pair();
        c[1].0.pinning.core = Some(3);
        assert_eq!(
            status(&aggregate(&c, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail),
            "AA-3 sets pinned to different cores must not be summed"
        );

        // A different governor → refused for the same reason.
        let mut g = pair();
        g[1].0.pinning.governor = "schedutil".into();
        assert_eq!(
            status(&aggregate(&g, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail)
        );

        // At AA-1, ONLY the migration probe (unpinned by design) may differ. Two NORMAL AA-1
        // condition sets on different cores are still a different measurement → refused.
        let mut a1 = [
            a_loaded_set(Stage::Aa1, "a", "pinned-solo", &"a".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "b", "co-tenant-other-core", &"b".repeat(64), 2),
        ];
        a1[1].0.pinning.core = Some(3);
        assert_eq!(
            status(&aggregate(&a1, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail),
            "normal AA-1 sets on different cores are not one measurement — only the probe is exempt"
        );

        // The migration probe, unpinned, aggregates fine beside the pinned condition set —
        // even though its posture differs, because it is the one sanctioned exception.
        let mut probe = [
            a_loaded_set(Stage::Aa1, "a", "pinned-solo", &"a".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "b", "migration-probe", &"b".repeat(64), 2),
        ];
        probe[1].0.pinning.pinned = false;
        probe[1].0.pinning.core = None;
        probe[1].0.pinning.migration_probe = true;
        assert_eq!(
            status(&aggregate(&probe, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Pass),
            "AA-1's migration probe runs unpinned by design — it alone may differ"
        );

        // And if the FIRST set is the probe, the normal sets are still held to a shared
        // posture (the reference is the first NON-probe set, not simply the first set).
        let mut probe_first = [
            a_loaded_set(Stage::Aa1, "a", "migration-probe", &"a".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "b", "pinned-solo", &"b".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "c", "co-tenant-other-core", &"c".repeat(64), 2),
        ];
        probe_first[0].0.pinning.pinned = false;
        probe_first[0].0.pinning.core = None;
        probe_first[0].0.pinning.migration_probe = true;
        probe_first[2].0.pinning.core = Some(3); // a normal set on a different core
        assert_eq!(
            status(
                &aggregate(&probe_first, &floors).outcomes,
                CheckId::Aggregation
            ),
            Some(Status::Fail),
            "a probe sorting first must not let the normal sets diverge in posture"
        );
    }

    #[test]
    fn an_all_probe_or_multi_probe_aa1_aggregate_is_rejected() {
        let floors = Floors {
            min_armed_overflows: Some(2),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };
        let make_probe = |set: &mut (RunSet, Vec<RunRecord>, Vec<u8>)| {
            set.0.pinning.pinned = false;
            set.0.pinning.core = None;
            set.0.pinning.migration_probe = true;
        };

        // Every set a probe → a fully unpinned matrix (and no non-probe reference).
        let mut all_probe = [
            a_loaded_set(Stage::Aa1, "p1", "pinned-solo", &"a".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "p2", "co-tenant-other-core", &"b".repeat(64), 2),
        ];
        all_probe.iter_mut().for_each(make_probe);
        assert_eq!(
            status(
                &aggregate(&all_probe, &floors).outcomes,
                CheckId::Aggregation
            ),
            Some(Status::Fail),
            "an all-probe AA-1 aggregate is a fully unpinned matrix"
        );

        // A single set that is itself a probe is also rejected (no pinned measurement at all).
        let mut single = [a_loaded_set(
            Stage::Aa1,
            "p",
            "pinned-solo",
            &"a".repeat(64),
            2,
        )];
        make_probe(&mut single[0]);
        assert_eq!(
            status(&aggregate(&single, &floors).outcomes, CheckId::Aggregation),
            Some(Status::Fail),
            "a lone probe is not a certification population"
        );

        // Two probes beside a pinned set → more than one bounded probe is refused.
        let mut two_probes = [
            a_loaded_set(Stage::Aa1, "solo", "pinned-solo", &"a".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "p1", "co-tenant-other-core", &"b".repeat(64), 2),
            a_loaded_set(Stage::Aa1, "p2", "memory-pressure", &"c".repeat(64), 2),
        ];
        make_probe(&mut two_probes[1]);
        make_probe(&mut two_probes[2]);
        assert_eq!(
            status(
                &aggregate(&two_probes, &floors).outcomes,
                CheckId::Aggregation
            ),
            Some(Status::Fail),
            "AA-1 permits exactly one bounded probe, not two"
        );
    }

    #[test]
    fn the_migration_probe_does_not_satisfy_condition_coverage() {
        // Part B: the probe's armed records must NOT count toward the pinned condition matrix.
        // `--sub-normative` relaxes the full grid + magnitude but keeps the per-condition
        // "nonzero armed" requirement, which is the exact axis this isolates.
        let sub = Floors {
            min_armed_overflows: Some(4),
            min_reps: None,
            min_cases: None,
            sub_normative: true,
        };
        // The full grid (4 pinned conditions + one probe) covers every condition → PASS.
        assert_eq!(
            status(
                &aggregate(&full_aa1_grid(), &sub).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Pass),
            "the four pinned conditions each carry armed records"
        );

        // Now drop the memory-pressure PINNED set (index 3) and relabel the probe (index 4) to
        // memory-pressure. If the probe counted, the condition would still read covered; because
        // it is excluded, memory-pressure has zero pinned armed records and the matrix fails.
        let mut grid = full_aa1_grid();
        grid[4].0.condition = "memory-pressure".into();
        for r in &mut grid[4].1 {
            r.condition = "memory-pressure".into();
        }
        grid.remove(3);
        assert_eq!(
            status(&aggregate(&grid, &sub).outcomes, CheckId::ConditionMatrix),
            Some(Status::Fail),
            "an unpinned probe cannot stand in for a required contamination condition"
        );
    }

    /// Build a swept AA-1 set (one payload, 1e6/1e7/1e8) that is also the migration probe.
    fn swept_migration_probe(id: &str, hash: &str) -> (RunSet, Vec<RunRecord>, Vec<u8>) {
        let mut set = a_loaded_set(Stage::Aa1, id, "pinned-solo", hash, 3);
        set.1[0].scale = Scale::S1e6;
        set.1[1].scale = Scale::S1e7;
        set.1[2].scale = Scale::S1e8;
        set.0.pinning.pinned = false;
        set.0.pinning.core = None;
        set.0.pinning.migration_probe = true;
        set
    }

    /// A COMPLETE AA-1 certification input: every required payload class at every
    /// contamination condition and every sweep scale (armed), plus an armed migration probe.
    fn full_aa1_grid() -> Vec<(RunSet, Vec<RunRecord>, Vec<u8>)> {
        let conditions = [
            ("c0", "pinned-solo", "a"),
            ("c1", "co-tenant-other-core", "b"),
            ("c2", "co-tenant-same-core", "c"),
            ("c3", "memory-pressure", "d"),
        ];
        let mut sets = Vec::new();
        for (id, cond, hash) in conditions {
            let mut set = a_loaded_set(Stage::Aa1, id, cond, &hash.repeat(64), 0);
            // This helper exercises the ConditionMatrix grid, not counts; `weights: None`
            // makes the count check refuse (NOT-REQUESTED) instead of simulating 1e8 trips per
            // cell — keeping the test fast while leaving the grid verdict unchanged.
            set.0.weights = None;
            let mut records = Vec::new();
            let mut sample_id = 0u64;
            for &p in REQUIRED_AA1_PAYLOADS {
                for &sc in REQUIRED_AA1_SWEEP_SCALES {
                    let mut r = a_record(sample_id);
                    r.payload = p;
                    r.scale = sc;
                    r.condition = cond.to_string();
                    records.push(r);
                    sample_id += 1;
                }
            }
            set.1 = records;
            sets.push(set);
        }
        let mut probe = swept_migration_probe("mig", &"e".repeat(64));
        probe.0.weights = None;
        sets.push(probe);
        sets
    }

    #[test]
    fn normative_aa1_requires_the_full_matrix_grid() {
        let normative = Floors {
            min_armed_overflows: Some(4),
            min_reps: None,
            min_cases: None,
            sub_normative: false,
        };

        // The complete grid + armed migration probe certifies.
        assert_eq!(
            status(
                &aggregate(&full_aa1_grid(), &normative).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Pass),
            "every class × condition × scale cell, plus the armed migration probe, certifies"
        );

        // Drop the WFI class entirely — a submission of only the other classes never measures
        // it, which the union-of-labels check missed but the grid catches.
        let mut no_wfi = full_aa1_grid();
        for (_, records, _) in &mut no_wfi {
            records.retain(|r| r.payload != Payload::WfiIdle);
        }
        assert_eq!(
            status(
                &aggregate(&no_wfi, &normative).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Fail),
            "a grid missing the WFI class is incomplete"
        );

        // Drop one condition for one class (svc only at pinned-solo): disjoint payloads per
        // condition give no per-class contamination comparison.
        let mut disjoint = full_aa1_grid();
        for (rs, records, _) in &mut disjoint {
            if rs.condition != "pinned-solo" {
                records.retain(|r| r.payload != Payload::Svc);
            }
        }
        assert_eq!(
            status(
                &aggregate(&disjoint, &normative).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Fail),
            "svc measured under only one condition has no contamination comparison"
        );

        // Smoke-only (no sweep scales) → fail; --sub-normative relaxes the whole grid.
        let mut smoke = full_aa1_grid();
        for (_, records, _) in &mut smoke {
            for r in records {
                r.scale = Scale::Smoke;
            }
        }
        assert_eq!(
            status(
                &aggregate(&smoke, &normative).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Fail),
            "smoke-only has no differential sweep for any class"
        );
        let sub = Floors {
            min_cases: None,
            sub_normative: true,
            ..normative
        };
        assert_eq!(
            status(&aggregate(&smoke, &sub).outcomes, CheckId::ConditionMatrix),
            Some(Status::Pass),
            "sub-normative relaxes the grid"
        );
    }

    #[test]
    fn aa1_migration_probe_must_contribute_armed_records() {
        let normative = Floors {
            min_armed_overflows: Some(4),
            min_reps: None,
            min_cases: None,
            sub_normative: false,
        };

        // The complete grid but with a COUNTING-MODE probe (unarmed) — labelled as the probe
        // but migrating nothing under an armed overflow. The grid stays complete (the pinned
        // conditions cover its cells), so the ONLY failure is the missing armed probe.
        let mut counting = full_aa1_grid();
        let probe = counting.last_mut().expect("grid has the probe set");
        for r in &mut probe.1 {
            r.overflow = None;
        }
        assert_eq!(
            status(
                &aggregate(&counting, &normative).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Fail),
            "a counting-mode probe set migrates nothing under an armed overflow"
        );

        // The same grid with the probe armed → passes.
        assert_eq!(
            status(
                &aggregate(&full_aa1_grid(), &normative).outcomes,
                CheckId::ConditionMatrix
            ),
            Some(Status::Pass),
            "an armed migration-probe set covers the rr #3607 experiment"
        );
    }

    #[test]
    fn an_uppercase_hash_is_schema_invalid_not_normalized_away() {
        // `normalise_hash` lowercases for the case-insensitive records-hash MATCH, but the
        // schema pattern is [0-9a-f]: an UPPERCASE sha256 is schema-invalid and must fail
        // well-formedness rather than being normalized into a pass.
        let mut rs = a_run_set();
        rs.records_sha256 = "A".repeat(64); // 64 uppercase hex
        let mut out = Vec::new();
        check_well_formed(&rs, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::WellFormed),
            Some(Status::Fail),
            "an uppercase hash is not canonical [0-9a-f]"
        );
    }

    #[test]
    fn aa1_armed_runs_must_use_the_stock_signal_mechanism() {
        // An armed AA-1 run declaring the patched Preempt path measures AA-3's mechanism,
        // not AA-1(c)'s pre-patch signal kick — refused.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        // a_record is armed and carries Preempt; the manifest declares the patched path.
        let mut out = Vec::new();
        check_mechanism(&rs, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Fail),
            "AA-1 armed through the patched Preempt path is the wrong mechanism"
        );

        // The stock signal-kick mechanism with signal-kick records → attested.
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
            Some(Status::Pass)
        );

        // A counting-mode AA-1 run (nothing armed) is exempt — it ends at the console
        // sentinel with no mechanism to attest.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa1;
        let mut r = a_record(0);
        r.overflow = None;
        r.exit_reason = ExitReason::Mmio;
        let mut out = Vec::new();
        check_mechanism(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::MechanismAttestation),
            Some(Status::Pass),
            "counting-mode AA-1 arms nothing, so the stock-mechanism rule does not bite"
        );
    }

    #[test]
    fn a_zero_attempt_run_set_is_refused_not_vacuously_passed() {
        // `attempted: 0` with an empty records file passes totality and every per-record
        // check vacuously — a verdict over a run that never happened. It must fail closed.
        let mut rs = a_run_set();
        rs.attempted = 0;
        let mut out = Vec::new();
        check_totality(&rs, &[], &mut out);
        assert_eq!(status(&out, CheckId::Totality), Some(Status::Fail));
    }

    #[test]
    fn a_record_whose_condition_contradicts_its_manifest_is_caught() {
        let rs = a_run_set(); // manifest condition = pinned-solo
        let mut r = a_record(0);
        r.condition = "co-tenant-other-core".into(); // a record claiming another condition
        let mut out = Vec::new();
        check_condition_consistency(&rs, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::ConditionConsistency),
            Some(Status::Fail)
        );
    }

    #[test]
    fn rep_floor_fails_below_the_minimum() {
        let floors = Floors {
            min_armed_overflows: None,
            min_reps: Some(1_000),
            min_cases: None,
            sub_normative: false,
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

        // An empty state_digest on a record fails — even an ARMED record that carries a
        // populated landed_digest (which the replay check would compare instead), because
        // the state_digest is a required, complete-state field in its own right.
        let mut armed = a_record(0); // a_record is armed with a landed_digest
        armed.state_digest = String::new();
        let mut out = Vec::new();
        check_well_formed(&a_run_set(), &[armed], &mut out);
        assert_eq!(
            status(&out, CheckId::WellFormed),
            Some(Status::Fail),
            "an empty state_digest is not rescued by a populated landed_digest"
        );

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
            min_cases: None,
            sub_normative: false,
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
    fn aa6_rep_floor_counts_only_injected_repetitions() {
        // AA-6's floor is over repetitions UNDER injection. One armed record beside three
        // `armed: false` records of the same input is not three injected reps.
        let mut rs = a_run_set();
        rs.stage = Stage::Aa6;
        let floors = Floors {
            min_armed_overflows: None,
            min_reps: Some(3),
            min_cases: None,
            sub_normative: true, // 3 < 1000: accept the weakened floor for the unit
        };
        let unarmed = |id: u64| {
            let mut r = a_record_seeded(id, 7);
            if let Some(o) = r.overflow.as_mut() {
                o.armed = false;
                o.deliveries = 0;
            }
            r
        };
        let one_injected = vec![
            a_record_seeded(0, 7), // armed + delivered
            unarmed(1),
            unarmed(2),
            unarmed(3),
        ];
        let mut out = Vec::new();
        check_floors(&rs, &floors, &one_injected, &mut out);
        assert_eq!(
            status(&out, CheckId::RepFloor),
            Some(Status::Fail),
            "one injected rep beside three unarmed records is not three reps under injection"
        );

        // Three actually-injected reps of the same input meet the floor.
        let three_injected = vec![
            a_record_seeded(0, 7),
            a_record_seeded(1, 7),
            a_record_seeded(2, 7),
        ];
        let mut out = Vec::new();
        check_floors(&rs, &floors, &three_injected, &mut out);
        assert_eq!(status(&out, CheckId::RepFloor), Some(Status::Pass));
    }

    #[test]
    fn distinct_case_coverage_binds_separately_from_the_deadline_total() {
        let armed_floor = |min_cases| Floors {
            min_armed_overflows: Some(1000),
            min_reps: None,
            min_cases,
            sub_normative: true,
        };

        // 1000 armed reps of ONE input = one distinct case (the --cases 1 --reps 1000 vacuity).
        let one_case: Vec<RunRecord> = (0..1000u64).map(|i| a_record_seeded(i, 7)).collect();
        assert_eq!(
            distinct_armed_cases(&one_case).len(),
            1,
            "1000 reps of one seed is one case, not 1000"
        );

        // No --min-cases → NOT-REQUESTED even with 1000 armed deadlines: never a silent pass.
        let mut out = Vec::new();
        check_case_coverage(Stage::Aa3, &armed_floor(None), 1, &mut out);
        assert_eq!(
            status(&out, CheckId::CaseCoverage),
            Some(Status::NotRequested),
            "the distinct-case coverage was never bounded"
        );

        // --min-cases 8 with only one distinct case → FAIL (deadlines met the floor by cloning).
        let mut out = Vec::new();
        check_case_coverage(Stage::Aa3, &armed_floor(Some(8)), 1, &mut out);
        assert_eq!(status(&out, CheckId::CaseCoverage), Some(Status::Fail));

        // Eight distinct cases meets a floor of 8 → PASS.
        let eight: Vec<RunRecord> = (0..8u64).map(|i| a_record_seeded(i, i + 1)).collect();
        assert_eq!(distinct_armed_cases(&eight).len(), 8);
        let mut out = Vec::new();
        check_case_coverage(Stage::Aa3, &armed_floor(Some(8)), 8, &mut out);
        assert_eq!(status(&out, CheckId::CaseCoverage), Some(Status::Pass));

        // A --min-cases floor of 0 is vacuous → FAIL.
        let mut out = Vec::new();
        check_case_coverage(Stage::Aa3, &armed_floor(Some(0)), 5, &mut out);
        assert_eq!(status(&out, CheckId::CaseCoverage), Some(Status::Fail));

        // Zero armed cases (a counting-mode run) → no case-coverage outcome at all.
        let mut out = Vec::new();
        check_case_coverage(Stage::Aa1, &armed_floor(Some(4)), 0, &mut out);
        assert!(
            out.is_empty(),
            "no armed deadlines → no target distribution to bound"
        );

        // A stage with no armed floor (AA-2) → no case-coverage outcome regardless.
        let mut out = Vec::new();
        check_case_coverage(Stage::Aa2, &armed_floor(Some(4)), 10, &mut out);
        assert!(out.is_empty(), "AA-2 has no target/case dimension");
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

    /// A record carrying one stepped measurement. A real single step lands on a Debug exit and
    /// arms no overflow, so the helper sets both (matching what the harness would emit).
    fn a_step(id: u64, pc_after: u64, delta: u64, transition: StepTransition) -> RunRecord {
        let mut r = a_record(id);
        r.exit_reason = ExitReason::Debug;
        r.overflow = None;
        r.step = Some(StepRecord {
            pc_before: 0x8000,
            pc_after,
            insn_retired: 1,
            br_retired_delta: delta,
            transition,
            step_digest: format!("sha256:step-{transition:?}"),
        });
        r
    }

    /// One valid step of each required transition class — a complete AA-2 matrix.
    fn full_step_matrix() -> Vec<RunRecord> {
        vec![
            a_step(0, 0x8004, 0, StepTransition::Sequential),
            a_step(1, 0x9000, 1, StepTransition::TakenBranch),
            a_step(2, 0xA000, 0, StepTransition::ExceptionEntry),
            a_step(3, 0xB000, 0, StepTransition::ExceptionReturn),
            a_step(4, 0xC000, 0, StepTransition::Wfi),
            a_step(5, 0xD000, 0, StepTransition::Injection),
            a_step(6, 0xE000, 0, StepTransition::LlscExclusive),
        ]
    }

    #[test]
    fn aa2_requires_a_valid_step_measurement_not_a_debug_label() {
        // No steps → NOT-REQUESTED (the run path is arrival-day), never a silent PASS.
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::NotRequested)
        );

        // A bare exit_reason=debug with no step measurement is not AA-2 evidence — it is now
        // REJECTED (a debug exit must carry a validated single step), not silently ignored.
        let mut labelled = a_record(0);
        labelled.exit_reason = ExitReason::Debug;
        labelled.overflow = None;
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &[labelled], &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Fail),
            "a debug exit without a step measurement is rejected, not ignored"
        );

        // A single valid step is NOT the AA-2 matrix — the coverage requirement (r10) fails
        // a partial set even when the one step it has is well-formed.
        let mut out = Vec::new();
        check_debug_evidence(
            Stage::Aa2,
            &[a_step(0, 0x8004, 0, StepTransition::Sequential)],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Fail),
            "one step is not stepping across the whole matrix"
        );

        // The FULL matrix, each step valid → PASS.
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &full_step_matrix(), &mut out);
        assert_eq!(status(&out, CheckId::DebugEvidence), Some(Status::Pass));

        // Now corrupt one class in the otherwise-complete matrix and confirm each is
        // caught. (a) a sequential step must not move BR_RETIRED.
        let mut m = full_step_matrix();
        m[0] = a_step(0, 0x8004, 99, StepTransition::Sequential);
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &m, &mut out);
        assert_eq!(status(&out, CheckId::DebugEvidence), Some(Status::Fail));

        // (b) a taken branch MUST increment BR_RETIRED by exactly 1.
        let mut m = full_step_matrix();
        m[1] = a_step(1, 0x9000, 0, StepTransition::TakenBranch);
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &m, &mut out);
        assert_eq!(status(&out, CheckId::DebugEvidence), Some(Status::Fail));

        // (c) an exception transition is classified from the OPCODE, not PC arithmetic:
        // its PC moves far (pc != pc+4) but that is NOT forced to be a retired branch —
        // the delta is measured (0 here), and the matrix passes. This is the r10 point:
        // an ERET / SVC / injected IRQ must not be forced to delta 1 by PC movement alone.
        let mut m = full_step_matrix();
        m[3] = a_step(3, 0xFFFF_0000, 0, StepTransition::ExceptionReturn);
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &m, &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Pass),
            "an exception transition with a far PC move and delta 0 is valid — not a forced branch"
        );

        // (d) a matrix that covers every OTHER class but no LL/SC exclusive is not AA-2:
        // the monitor-clearing/livelock characterization AA-4 needs was never stepped.
        let without_llsc: Vec<RunRecord> = full_step_matrix()
            .into_iter()
            .filter(|r| {
                r.step.as_ref().map(|s| s.transition) != Some(StepTransition::LlscExclusive)
            })
            .collect();
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &without_llsc, &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Fail),
            "an AA-2 run that never stepped an LL/SC exclusive is incomplete"
        );

        // (e) a sequential step must land at EXACTLY PC+4 — a jump of 8 skipped an
        // instruction, which is the miss AA-2 exists to catch, even with delta 0.
        let mut m = full_step_matrix();
        m[0] = a_step(0, 0x8008, 0, StepTransition::Sequential); // pc_before 0x8000 → +8
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &m, &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Fail),
            "a sequential step that advances by 8, not 4, skipped an instruction"
        );

        // (f) an LL/SC exclusive step is a load/store, not a taken branch: BR_RETIRED must
        // not move, so a delta of 1 (the exception/WFI class's allowance) is rejected here.
        let mut m = full_step_matrix();
        m[6] = a_step(6, 0xE000, 1, StepTransition::LlscExclusive);
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &m, &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Fail),
            "a single-stepped LDXR/STXR is not a taken branch; delta 1 is invalid"
        );

        // The check does not fire outside AA-2.
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa3, &[a_record(0)], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn aa2_replay_identity_compares_step_moment_digests_across_repeats() {
        // A step record for a chosen input seed and step-moment digest.
        let step_of = |id: u64, seed: u64, digest: &str| {
            let mut r = a_record_seeded(id, seed);
            r.overflow = None; // a stepped record is not armed (they are mutually exclusive)
            r.step = Some(StepRecord {
                pc_before: 0x8000,
                pc_after: 0x8004,
                insn_retired: 1,
                br_retired_delta: 0,
                transition: StepTransition::Sequential,
                step_digest: digest.into(),
            });
            r
        };

        // AA-2's acceptance is replay-identical STEPPED states. requires_replay_identity
        // now includes AA-2, so DISTINCT inputs (no repeated group) read NOT-REQUESTED.
        let distinct = [step_of(0, 1, "sha256:a"), step_of(1, 2, "sha256:b")];
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa2, &distinct, &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested),
            "distinct inputs have no repeated group to replay"
        );

        // Two reps of the SAME stepped input (same RepKey) with the SAME step_digest →
        // replay identity holds.
        let mk = |id: u64, digest: &str| {
            let mut r = a_record_seeded(id, 7);
            r.overflow = None; // a stepped record is not armed (they are mutually exclusive)
            r.step = Some(StepRecord {
                pc_before: 0x8000,
                pc_after: 0x8004,
                insn_retired: 1,
                br_retired_delta: 0,
                transition: StepTransition::Sequential,
                step_digest: digest.into(),
            });
            r
        };
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa2,
            &[mk(0, "sha256:same"), mk(1, "sha256:same")],
            &mut out,
        );
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Pass));

        // Divergent step-moment states that would CONVERGE by the exit (same
        // state_digest) are caught because the STEP digest, not the final one, is compared.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa2,
            &[mk(0, "sha256:diverged-a"), mk(1, "sha256:diverged-b")],
            &mut out,
        );
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));
    }

    #[test]
    fn two_step_moments_of_one_input_are_not_false_divergence() {
        // Two DIFFERENT step points of the same input — an SVC exception entry and its ERET —
        // share (payload, scale, seed, condition) and carry no target. Without the step moment
        // in the RepKey they group together and their necessarily-different step_digests read
        // as false divergence. With it, each point groups with its OWN repetitions and, when
        // each is individually replay-identical, the check passes.
        let step_at = |id: u64, pc: u64, tr: StepTransition, digest: &str| {
            let mut r = a_record_seeded(id, 7); // same seed for all four
            r.step = Some(StepRecord {
                pc_before: pc,
                pc_after: pc + 4,
                insn_retired: 1,
                br_retired_delta: 0,
                transition: tr,
                step_digest: digest.into(),
            });
            r
        };
        // Point A (exception entry) repeated twice, point B (ERET) repeated twice; each
        // point's two reps are bit-identical, the two points differ.
        let records = [
            step_at(0, 0x1000, StepTransition::ExceptionEntry, "sha256:entry"),
            step_at(1, 0x1000, StepTransition::ExceptionEntry, "sha256:entry"),
            step_at(2, 0x2000, StepTransition::ExceptionReturn, "sha256:eret"),
            step_at(3, 0x2000, StepTransition::ExceptionReturn, "sha256:eret"),
        ];
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa2, &records, &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::Pass),
            "two distinct step moments of one input, each individually replay-identical, are \
             not divergence"
        );
    }

    #[test]
    fn replay_identity_is_not_satisfied_by_an_unrelated_repeated_group() {
        // AA-2: the review's scenario — one stepped record (singleton) beside two duplicate
        // UNSTEPPED records. The unstepped pair must NOT make the stepped state's replay
        // identity "pass"; with no repeated stepped group it reads NOT-REQUESTED.
        let stepped = |id: u64, seed: u64| {
            let mut r = a_record_seeded(id, seed);
            r.step = Some(StepRecord {
                pc_before: 0x8000,
                pc_after: 0x8004,
                insn_retired: 1,
                br_retired_delta: 0,
                transition: StepTransition::Sequential,
                step_digest: "sha256:step".into(),
            });
            r
        };
        let unstepped = |id: u64, seed: u64| {
            let mut r = a_record_seeded(id, seed);
            r.step = None;
            r
        };
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa2,
            &[stepped(0, 1), unstepped(1, 2), unstepped(2, 2)],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested),
            "a lone stepped record is unreplayed; an unstepped repeated group is no stand-in"
        );

        // AA-3: a lone armed landing beside a repeated UNARMED group — same vacuity.
        let armed = a_record_seeded(0, 1); // a_record is armed, delivered
        let unarmed = |id: u64, seed: u64| {
            let mut r = a_record_seeded(id, seed);
            r.overflow = None;
            r.exit_reason = ExitReason::Mmio;
            r
        };
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa3, &[armed, unarmed(1, 2), unarmed(2, 2)], &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested),
            "a lone armed landing is unreplayed; an unarmed repeated group is no stand-in"
        );
    }

    #[test]
    fn aa3_replay_requires_every_armed_landing_replayed_not_just_some() {
        // Two armed landings replayed (seed 7 pair) but a third armed landing (seed 9) left
        // as a singleton: partial coverage is a real gap → FAIL.
        let mixed = [
            a_record_seeded(0, 7),
            a_record_seeded(1, 7),
            a_record_seeded(2, 9),
        ];
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa3, &mixed, &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::Fail),
            "an armed landing left as a singleton beside a replayed group is a coverage gap"
        );

        // Every armed group replayed → PASS.
        let all_paired = [
            a_record_seeded(0, 7),
            a_record_seeded(1, 7),
            a_record_seeded(2, 9),
            a_record_seeded(3, 9),
        ];
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa3, &all_paired, &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Pass));
    }

    #[test]
    fn aa6_replay_compares_the_final_state_not_the_landed_digest() {
        // Two AA-6 injections of one input land IDENTICALLY at the injection Moment (same
        // landed_digest) but diverge PROCESSING the event (different state_digest). AA-6's
        // acceptance is the final state, so it must FAIL; AA-3 (which compares the landing)
        // would pass the same records — the whole point of selecting the digest by stage.
        let mk = |id: u64, state: &str| {
            let mut r = a_record_seeded(id, 7);
            if let Some(o) = r.overflow.as_mut() {
                o.landed_digest = "sha256:landed".into();
            }
            r.state_digest = state.to_string();
            r
        };
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa6,
            &[mk(0, "sha256:final-a"), mk(1, "sha256:final-b")],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::Fail),
            "identical landings that diverge processing the event are not AA-6 replay-identical"
        );
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa3,
            &[mk(0, "sha256:final-a"), mk(1, "sha256:final-b")],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::Pass),
            "AA-3 compares the landed digest, which is identical here"
        );
    }

    #[test]
    fn aa5_requires_both_clock_page_and_linux_guest_each_repeated() {
        let rec = |id: u64, seed: u64, payload: Payload, state: &str| {
            let mut r = a_record_seeded(id, seed);
            r.payload = payload;
            r.overflow = None; // AA-5's acceptance-bearing records are counting-mode
            r.state_digest = state.to_string();
            r
        };
        let cp = |id, seed, state| rec(id, seed, Payload::ClockPage, state);
        let lg = |id, seed, state| rec(id, seed, Payload::LinuxGuest, state);

        // Repeated clock-page runs but NO Linux guest → NOT-REQUESTED: half of AA-5 (the guest
        // boot AA-5(c) requires) was never exercised, so this cannot read as a PASS.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa5,
            &[cp(0, 7, "sha256:same"), cp(1, 7, "sha256:same")],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested),
            "repeated clock-page alone never boots the Linux guest AA-5 also certifies"
        );

        // Repeated Linux guest but NO clock page → likewise NOT-REQUESTED.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa5,
            &[lg(0, 7, "sha256:g"), lg(1, 7, "sha256:g")],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested)
        );

        // Both classes present but each a single rep → NOT-REQUESTED (nothing replayed).
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa5,
            &[cp(0, 7, "sha256:c"), lg(1, 9, "sha256:g")],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested)
        );

        // Both classes, each repeated bit-identically → PASS.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa5,
            &[
                cp(0, 7, "sha256:c"),
                cp(1, 7, "sha256:c"),
                lg(2, 9, "sha256:g"),
                lg(3, 9, "sha256:g"),
            ],
            &mut out,
        );
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Pass));

        // Both present and repeated, but the clock-page reps DIVERGE → FAIL.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa5,
            &[
                cp(0, 7, "sha256:a"),
                cp(1, 7, "sha256:b"),
                lg(2, 9, "sha256:g"),
                lg(3, 9, "sha256:g"),
            ],
            &mut out,
        );
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));

        // Both classes repeated, but one extra clock-page rep is a singleton group → FAIL:
        // EVERY acceptance-bearing group must be repeated, not just some.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa5,
            &[
                cp(0, 7, "sha256:c"),
                cp(1, 7, "sha256:c"),
                cp(2, 5, "sha256:solo"),
                lg(3, 9, "sha256:g"),
                lg(4, 9, "sha256:g"),
            ],
            &mut out,
        );
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));
    }

    #[test]
    fn aa6_matrix_counts_injected_records_not_payload_labels() {
        let required = required_aa6_classes();
        let full_armed = || -> Vec<RunRecord> {
            required
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    let mut r = a_record(i as u64); // armed + delivered
                    r.payload = p;
                    r
                })
                .collect()
        };
        // Every required class has an injected (armed, delivered) record → PASS.
        let mut out = Vec::new();
        check_aa6_matrix(Stage::Aa6, &full_armed(), &mut out);
        assert_eq!(status(&out, CheckId::Aa6Matrix), Some(Status::Pass));
        // Make one required class present only as an UNARMED record — it injected nothing.
        let mut records = full_armed();
        for r in &mut records {
            if r.payload == Payload::BranchDense {
                r.overflow = None;
            }
        }
        let mut out = Vec::new();
        check_aa6_matrix(Stage::Aa6, &records, &mut out);
        assert_eq!(
            status(&out, CheckId::Aa6Matrix),
            Some(Status::Fail),
            "a class present only as unarmed records injected nothing across the matrix"
        );
    }

    #[test]
    fn aa6_armed_run_needs_no_armed_overflow_floor() {
        // AA-6's floor is ≥1000 reps and its arming is a matrix invariant, NOT a numeric armed
        // floor. An armed AA-6 run with no --min-armed-overflows must emit NO floor outcome.
        let floors = Floors {
            min_armed_overflows: None,
            min_reps: Some(4),
            min_cases: None,
            sub_normative: true,
        };
        let mut out = Vec::new();
        check_armed_floor(Stage::Aa6, &floors, 40, &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            None,
            "AA-6 defines no armed floor — armed records without one are not INCOMPLETE"
        );
        // AA-1, which DOES define one, still reads NOT-REQUESTED without a floor.
        let mut out = Vec::new();
        check_armed_floor(Stage::Aa1, &floors, 40, &mut out);
        assert_eq!(
            status(&out, CheckId::ArmedOverflowFloor),
            Some(Status::NotRequested)
        );
    }

    #[test]
    fn aa4_requires_replay_evidence_for_the_lse_landing() {
        assert!(
            requires_replay_identity(Stage::Aa4),
            "AA-4's acceptance is the LSE-only landing replayed under the same injection schedule"
        );
        // AA-4's acceptance-bearing record is an armed LSE-payload landing — a straight-line
        // landing is NOT one, so it cannot certify the atomics contract.
        let lse = |id: u64, seed: u64| {
            let mut r = a_record_seeded(id, seed);
            r.payload = Payload::LseAtomics;
            r
        };

        // Repeated straight-line landings are not acceptance-bearing at AA-4 → NOT-REQUESTED.
        let mut out = Vec::new();
        check_replay_identity(
            Stage::Aa4,
            &[a_record_seeded(0, 7), a_record_seeded(1, 7)],
            &mut out,
        );
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested),
            "repeated straight-line landings are not the LSE experiment AA-4 certifies"
        );

        // A single armed LSE landing → nothing replayed → NOT-REQUESTED, not a silent PASS.
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa4, &[lse(0, 7)], &mut out);
        assert_eq!(
            status(&out, CheckId::ReplayIdentity),
            Some(Status::NotRequested),
            "a singleton AA-4 run demonstrated no injection-schedule invariance"
        );
        // Two bit-identical armed LSE reps of the same input → PASS.
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa4, &[lse(0, 7), lse(1, 7)], &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Pass));
        // Two LSE reps whose landed digests DIVERGE → FAIL.
        let mut a = lse(0, 7);
        let mut b = lse(1, 7);
        if let Some(o) = a.overflow.as_mut() {
            o.landed_digest = "sha256:aa".into();
        }
        if let Some(o) = b.overflow.as_mut() {
            o.landed_digest = "sha256:bb".into();
        }
        let mut out = Vec::new();
        check_replay_identity(Stage::Aa4, &[a, b], &mut out);
        assert_eq!(status(&out, CheckId::ReplayIdentity), Some(Status::Fail));

        // check_aa4_contract always reads NOT-REQUESTED (enforcement levels + ruling are
        // arrival-day), so AA-4 never PASSes on landings alone.
        let mut out = Vec::new();
        check_aa4_contract(Stage::Aa4, &[lse(0, 7), lse(1, 7)], &mut out);
        assert_eq!(
            status(&out, CheckId::Aa4Contract),
            Some(Status::NotRequested),
            "AA-4's enforcement levels and ruling are arrival-day — never a pass pre-silicon"
        );
    }

    #[test]
    fn a_record_with_both_a_step_and_an_armed_overflow_is_malformed() {
        // The modes are mutually exclusive: a record is a single-step OR an armed landing.
        // A crafted record with both would let its step_digest stand in for replay while its
        // divergent landed digests went unchecked, so it is rejected as malformed.
        let mut r = a_record(0); // a_record carries an armed, delivered overflow
        r.step = Some(StepRecord {
            pc_before: 0x8000,
            pc_after: 0x8004,
            insn_retired: 1,
            br_retired_delta: 0,
            transition: StepTransition::Sequential,
            step_digest: "sha256:s".into(),
        });
        let mut out = Vec::new();
        check_well_formed(&a_run_set(), &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::WellFormed),
            Some(Status::Fail),
            "a record carrying both a step and an armed overflow is two mechanisms in one"
        );
    }

    #[test]
    fn a_step_matrix_must_carry_debug_exits() {
        // A complete, well-formed step matrix whose records claim exit_reason=mmio was not
        // produced by KVM_GUESTDBG_SINGLESTEP — binding the exit label to the step catches it.
        let mut matrix = full_step_matrix();
        for r in &mut matrix {
            r.exit_reason = ExitReason::Mmio;
        }
        let mut out = Vec::new();
        check_debug_evidence(Stage::Aa2, &matrix, &mut out);
        assert_eq!(
            status(&out, CheckId::DebugEvidence),
            Some(Status::Fail),
            "a step matrix labelled mmio is not KVM_GUESTDBG_SINGLESTEP evidence"
        );
    }

    #[test]
    fn records_file_must_stay_inside_the_run_set_dir() {
        for ok in ["records.jsonl", "./records.jsonl", "sub/records.jsonl"] {
            assert!(records_file_is_confined(ok), "{ok} should be confined");
        }
        for bad in [
            "",
            "../records.jsonl",
            "../../etc/passwd",
            "/etc/passwd",
            "sub/../../escape",
            "a/../../../b",
        ] {
            assert!(!records_file_is_confined(bad), "{bad} must be rejected");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_records_file_resolving_outside_is_rejected() {
        // The lexical check passes a plain name like "records.jsonl", but if that name is a
        // SYMLINK to a file outside the run-set directory, `std::fs::read` would follow it and
        // hash external bytes as retained evidence. load_run_set must refuse on the resolved
        // path. (Unix-only: the test plants a symlink.)
        let base = std::env::temp_dir().join(format!("floorcheck-symlink-{}", std::process::id()));
        let inside = base.join("rs");
        let outside = base.join("outside");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let evil = outside.join("evil.jsonl");
        std::fs::write(&evil, b"").unwrap();

        let mut rs = a_run_set();
        rs.records_file = "records.jsonl".into();
        std::fs::write(inside.join(MANIFEST_FILE), serde_json::to_vec(&rs).unwrap()).unwrap();
        std::os::unix::fs::symlink(&evil, inside.join("records.jsonl")).unwrap();

        let err = load_run_set(&inside).unwrap_err();
        assert!(
            matches!(err, LoadError::RecordsPathEscapesDir { .. }),
            "a symlink resolving outside the run-set dir must be refused, got {err:?}"
        );

        // A REAL records file at the same name loads fine (containment holds).
        std::fs::remove_file(inside.join("records.jsonl")).unwrap();
        std::fs::write(inside.join("records.jsonl"), b"").unwrap();
        assert!(
            load_run_set(&inside).is_ok(),
            "an ordinary records.jsonl inside the dir must load"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn image_pins_bind_every_exercised_class_and_the_kernel() {
        // a_run_set: one image ("img", hash 0…0) matching host_kernel_sha256; a_record is
        // StraightLine, which has no image pin → FAIL.
        let mut out = Vec::new();
        check_image_pins(&a_run_set(), &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::ImagePins),
            Some(Status::Fail),
            "the exercised straight-line class has no image pin"
        );
        // Add a verified straight-line image → PASS (kernel matches, class pinned).
        let mut rs = a_run_set();
        rs.images.push(ImagePin {
            path: "payloads/straight-line".into(),
            sha256: "a".repeat(64),
            md5: None,
            verified_before_boot: true,
        });
        let mut out = Vec::new();
        check_image_pins(&rs, &[a_record(0)], &mut out);
        assert_eq!(status(&out, CheckId::ImagePins), Some(Status::Pass));
        // A kernel hash matching NO verified image → FAIL.
        let mut rs2 = rs.clone();
        rs2.mechanism.host_kernel_sha256 = "f".repeat(64);
        let mut out = Vec::new();
        check_image_pins(&rs2, &[a_record(0)], &mut out);
        assert_eq!(
            status(&out, CheckId::ImagePins),
            Some(Status::Fail),
            "the mechanism kernel identity matches no verified image pin"
        );
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
    fn an_overflowing_oracle_prediction_fails_closed() {
        // Malformed evidence with huge weights makes the checked oracle total overflow u64.
        // A saturated u64::MAX would be MATCHED by a record whose measured_taken is u64::MAX;
        // the checked total returns None and count exactness fails closed instead.
        let mut r = a_record(0);
        r.payload = Payload::Svc; // has exception/svc terms the weights multiply
        r.scale = Scale::S1e8;
        r.work_begin = 0;
        r.work_end = u64::MAX;
        r.measured_taken = u64::MAX;
        let huge = Weights::measured(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        let mut out = Vec::new();
        check_counts(&huge, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::CountExactness),
            Some(Status::Fail),
            "an unrepresentable prediction is malformed evidence, not a valid count"
        );
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
        check_multiplicity(&a_run_set(), &[r], &mut out);
        assert_eq!(status(&out, CheckId::Multiplicity), Some(Status::Fail));
    }

    #[test]
    fn the_migration_probe_records_lost_pmis_as_its_signature_not_a_failure() {
        // A lost PMI (deliveries == 0) fails multiplicity for a normal (pinned) set...
        let mut r = a_record(0);
        if let Some(o) = r.overflow.as_mut() {
            o.deliveries = 0;
        }
        let mut out = Vec::new();
        check_multiplicity(&a_run_set(), &[r.clone()], &mut out);
        assert_eq!(
            status(&out, CheckId::Multiplicity),
            Some(Status::Fail),
            "a pinned set must land exactly once"
        );

        // ...but for the MIGRATION PROBE it is the expected rr #3607 signature → PASS.
        let mut probe = a_run_set();
        probe.pinning.pinned = false;
        probe.pinning.core = None;
        probe.pinning.migration_probe = true;
        let mut out = Vec::new();
        check_multiplicity(&probe, &[r], &mut out);
        assert_eq!(
            status(&out, CheckId::Multiplicity),
            Some(Status::Pass),
            "the probe EXISTS to record a migration-lost PMI — grading it a failure is the \
             internal contradiction r24 flags"
        );

        // A DOUBLE delivery is anomalous even for the probe → FAIL.
        let mut dup = a_record(1);
        if let Some(o) = dup.overflow.as_mut() {
            o.deliveries = 2;
        }
        let mut out = Vec::new();
        check_multiplicity(&probe, &[dup], &mut out);
        assert_eq!(status(&out, CheckId::Multiplicity), Some(Status::Fail));
    }

    #[test]
    fn distinct_armed_cases_dedup_across_contamination_conditions() {
        // The r24 hole: four same-seed condition run-sets draw IDENTICAL cases. Because the
        // case key omits `condition`, four records that differ ONLY in condition are ONE case,
        // so four conditions × one target no longer satisfies --min-cases 4.
        let one_case_four_conditions: Vec<RunRecord> = [
            "pinned-solo",
            "co-tenant-other-core",
            "co-tenant-same-core",
            "memory-pressure",
        ]
        .iter()
        .enumerate()
        .map(|(i, cond)| {
            let mut r = a_record_seeded(i as u64, 7); // same seed, same payload/scale/target
            r.condition = (*cond).into();
            r
        })
        .collect();
        assert_eq!(
            distinct_armed_cases(&one_case_four_conditions).len(),
            1,
            "one case run under four conditions is one case, not four"
        );

        // Genuinely distinct seeds ARE distinct cases regardless of condition.
        let four_seeds: Vec<RunRecord> = (0..4u64).map(|i| a_record_seeded(i, i + 1)).collect();
        assert_eq!(distinct_armed_cases(&four_seeds).len(), 4);

        // End to end: the four-condition, one-case set fails --min-cases 4.
        let mut out = Vec::new();
        check_case_coverage(
            Stage::Aa1,
            &Floors {
                min_armed_overflows: Some(4),
                min_reps: None,
                min_cases: Some(4),
                sub_normative: true,
            },
            distinct_armed_cases(&one_case_four_conditions).len() as u64,
            &mut out,
        );
        assert_eq!(status(&out, CheckId::CaseCoverage), Some(Status::Fail));
    }
}
