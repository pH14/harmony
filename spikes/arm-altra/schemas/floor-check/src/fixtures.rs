// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fixture generator.
//!
//! Thirty-one checked-in run-sets: twenty-four the checker must reject (one per failure
//! mode) and seven it must accept (a patched AA-3 landing run, an AA-1 counting run, an
//! AA-1(c) early/late skid-distribution run, an AA-1 LL/SC-hazard run, an AA-6 same-input
//! gate, an AA-2 single-step transition matrix, and an AA-2 BOUNDED (`--max-steps`) matrix
//! whose step records' window counts are exempt from the oracle). They are **generated from
//! the oracle model**, not hand
//! written: the accept fixture's counts are the exact values
//! [`oracle_model::expected`] predicts under a chosen (synthetic) weights pack, so
//! the fixtures stay consistent with the model as it evolves, and a reject fixture
//! is the accept fixture with exactly one field mutated — which is what lets a test
//! assert *which* check catches it, not merely that something did.
//!
//! [`all_fixtures`] returns the bytes; the `gen-fixtures` binary writes them under
//! `schemas/fixtures/`, and the integration tests read them back and run the
//! checker. A drift test regenerates them in memory and asserts the committed files
//! still match, so a model change that would silently invalidate a fixture fails
//! the build instead.
//!
//! # Synthetic, and labelled as such
//!
//! Every hash here is a sha256 of a label, every count is derived, and the weights
//! and skid margin are placeholders chosen only so the manifest carries *some*
//! measured value (the checker's real job is to refuse a manifest that carries
//! *none*). Nothing in this module is a measurement; it is the shape a real
//! stage's evidence will take, filled with numbers a model — not silicon —
//! produced.

use arm_harness::evidence::{
    Environment, ExitReason, ImagePin, Mechanism, OverflowRecord, PerfConfig, Pinning, RunRecord,
    RunSet, SCHEMA_VERSION, Stage, StepRecord, StepTransition, hex_lower,
};
use oracle_model::{DEFAULT_SEED, Payload, Scale, Weights, expected};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// One generated fixture: a directory name and the two files' contents.
#[derive(Clone, Debug)]
pub struct Fixture {
    /// The fixture directory under `schemas/fixtures/`.
    pub name: &'static str,
    /// The `run-set.json` bytes.
    pub manifest_json: String,
    /// The `records.jsonl` bytes.
    pub records_jsonl: String,
}

/// The synthetic weights the fixtures predict counts under.
///
/// The "architecturally expected" answer — only branch instructions count, a
/// window offset of 2 (x86's analogue measured `n+2`) — but reached here by fiat
/// for a *fixture*, never assumed by the checker, which reads whatever the manifest
/// carries and refuses a manifest that carries nothing.
fn synthetic_weights() -> Weights {
    Weights::measured(0, 0, 0, 0, 2)
}

/// The synthetic skid margin the fixtures carry. A placeholder, so the manifest has
/// *a* measured margin; the real one is AA-1's to produce.
const SYNTHETIC_SKID_MARGIN: u64 = 64;

/// The synthetic overflow period every armed fixture record shares — a uniform
/// deadline `work_begin + SYNTHETIC_PERIOD`, so the manifest's single `sample_period`
/// is a truthful uniform claim.
const SYNTHETIC_PERIOD: u64 = 500;

/// The eight windowed payload classes, in a stable order. [`Payload::Ident`] is
/// excluded: it has no counting window, so it is not an armed-overflow sample.
const WINDOWED: [Payload; 8] = [
    Payload::StraightLine,
    Payload::BranchDense,
    Payload::Svc,
    Payload::ExceptionAbort,
    Payload::WfiIdle,
    Payload::LlscAtomics,
    Payload::LseAtomics,
    Payload::ClockPage,
];

/// A deterministic synthetic sha256 (64 hex) from a label.
fn synth_sha256(label: &str) -> String {
    let mut h = Sha256::new();
    h.update(label.as_bytes());
    hex_lower(&h.finalize())
}

/// A deterministic synthetic md5-shaped hash (32 hex) from a label.
fn synth_md5(label: &str) -> String {
    synth_sha256(label)[..32].to_string()
}

/// The environment every fixture shares — a plausible Altra, invented.
fn synthetic_environment() -> Environment {
    let mut firmware = BTreeMap::new();
    firmware.insert("scp".to_string(), "2.10.20221020".to_string());
    firmware.insert("sys".to_string(), "TianoCore-2.06.20230314".to_string());
    Environment {
        // 0x41 = Arm, 0xd0c = Neoverse N1.
        midr: 0x413f_d0c0,
        soc: "Ampere Altra Q80-30 (synthetic)".to_string(),
        firmware,
        host_kernel: "6.18.35-arm64-det".to_string(),
        kvm_mode: "vhe".to_string(),
    }
}

/// The boot artifacts every fixture pins. All verified, in the accept case: the host kernel
/// (its hash matching `mechanism.host_kernel_sha256`) plus one image per exercised payload
/// class (the file name is the class name, which the checker binds each exercised artifact
/// to), including the AA-5/AA-6 Linux guest.
fn synthetic_images() -> Vec<ImagePin> {
    let mut images = vec![ImagePin {
        path: "/boot/Image.det".to_string(),
        sha256: synth_sha256("host-kernel-image"),
        md5: Some(synth_md5("host-kernel-image")),
        verified_before_boot: true,
    }];
    for p in WINDOWED.iter().chain(std::iter::once(&Payload::LinuxGuest)) {
        images.push(ImagePin {
            path: format!("payloads/{}", p.name()),
            sha256: synth_sha256(&format!("payload-{}", p.name())),
            md5: Some(synth_md5(p.name())),
            verified_before_boot: true,
        });
    }
    images
}

fn synthetic_perf() -> PerfConfig {
    PerfConfig {
        // 0x21 = BR_RETIRED (all architecturally executed branch instructions on N1, AA1-F1).
        raw_event: 0x21,
        exclude_host: true,
        exclude_guest: false,
        exclude_hv: true,
        pinned: true,
        sample_period: Some(SYNTHETIC_PERIOD),
    }
}

fn synthetic_pinning() -> Pinning {
    Pinning {
        pinned: true,
        core: Some(2),
        governor: "performance".to_string(),
        migration_probe: false,
    }
}

/// The mechanism a patched AA-3 run claims: the in-kernel `Preempt` exit, observed.
fn patched_mechanism() -> Mechanism {
    Mechanism {
        kvm_patched: true,
        host_kernel_sha256: synth_sha256("host-kernel-image"),
        expected_exit_reason: ExitReason::Preempt,
        patch_marker_observed: true,
    }
}

/// The mechanism a stock AA-1 run claims: the pre-patch host-side `SignalKick`.
fn stock_mechanism() -> Mechanism {
    Mechanism {
        kvm_patched: false,
        host_kernel_sha256: synth_sha256("host-kernel-stock"),
        expected_exit_reason: ExitReason::SignalKick,
        patch_marker_observed: false,
    }
}

/// One armed-overflow record, generated so its count is exactly what the oracle
/// predicts under [`synthetic_weights`] and its landing is exact (skid 0).
fn generate_record(sample_id: u64, payload: Payload, exit: ExitReason) -> RunRecord {
    let scale = Scale::Smoke;
    let seed = DEFAULT_SEED;
    let e = expected(payload, scale, seed);
    let reported_taken = 0;
    let measured_taken = e
        .total(&synthetic_weights(), reported_taken)
        .expect("synthetic fixture weights are small and do not overflow");
    let work_begin = 1_000;
    let work_end = work_begin + measured_taken;
    // The overflow deadline is `work_begin + SYNTHETIC_PERIOD`, a UNIFORM period across
    // every record, decoupled from the window count. That is what lets the manifest
    // carry one `sample_period` truthfully; a real varying-period run would carry
    // `null` and let each record state its own (target - work_begin).
    let target = work_begin + SYNTHETIC_PERIOD;
    // The token the guest prints when the harness published its STATIC page
    // (`payloads/runtime/src/pvclock.rs`): `managed-static` — the publication plumbing
    // works, but it is not the `work-derived` clock AA-5 certifies (that mechanism is
    // `hm-8h8`). The AA-5 check reads this field; a fixture that invented a token no
    // guest can emit would be testing a fiction.
    let clockpage_mode = if payload == Payload::ClockPage {
        Some("managed-static".to_string())
    } else {
        None
    };
    RunRecord {
        sample_id,
        payload,
        scale,
        seed,
        trips: e.trips,
        condition: "pinned-solo".to_string(),
        work_begin,
        work_end,
        measured_taken,
        reported_taken,
        exit_reason: exit,
        overflow: Some(OverflowRecord {
            armed: true,
            deliveries: 1,
            advisory_exits: 0,
            target,
            landed: target,
            skid: 0,
            landed_digest: format!(
                "sha256:{}",
                synth_sha256(&format!("landed-{sample_id}-{}", payload.name()))
            ),
        }),
        // No fixture is a stepped AA-2 run (the run path is arrival-day).
        step: None,
        state_digest: format!(
            "sha256:{}",
            synth_sha256(&format!("state-{sample_id}-{}", payload.name()))
        ),
        params_mode: "managed".to_string(),
        clockpage_mode,
        payload_status: 0,
    }
}

/// The eight base records for a run-set with the given exit reason.
fn base_records(exit: ExitReason) -> Vec<RunRecord> {
    WINDOWED
        .iter()
        .enumerate()
        .map(|(i, &p)| generate_record(i as u64, p, exit))
        .collect()
}

/// Serialise records to `records.jsonl`: one compact JSON object per line.
fn records_jsonl(records: &[RunRecord]) -> String {
    let mut s = String::new();
    for r in records {
        // Serialising a concrete evidence record to JSON is statically infallible:
        // the shapes contain no floats and no non-string map keys, and a `String`
        // writer cannot fail.
        s.push_str(&serde_json::to_string(r).expect("record JSON is infallible"));
        s.push('\n');
    }
    s
}

/// Build a manifest for the given records, pinning the records' real sha256.
fn build_run_set(stage: Stage, mechanism: Mechanism, records: &[RunRecord]) -> RunSet {
    let jsonl = records_jsonl(records);
    let sha = synth_sha256_of_bytes(jsonl.as_bytes());
    // The host-kernel IMAGE pin's hash must equal the mechanism block's host_kernel_sha256
    // (the checker cross-checks the kernel identity against a verified pin), so bind the
    // first image (the kernel) to whatever kernel this mechanism claims.
    let mut images = synthetic_images();
    images[0].sha256 = mechanism.host_kernel_sha256.clone();
    // For step evidence one planned sample emits many step records, so `planned` is the number
    // of distinct stable planned-sample ids; for ordinary evidence each record is a sample.
    let planned = if records.iter().any(|r| r.step.is_some()) {
        records
            .iter()
            .filter_map(|r| r.step.as_ref().map(|s| s.planned_sample_id))
            .collect::<BTreeSet<_>>()
            .len() as u64
    } else {
        records.len() as u64
    };
    RunSet {
        schema_version: SCHEMA_VERSION,
        stage,
        run_set_id: format!("fixture-{}", stage_slug(stage)),
        environment: synthetic_environment(),
        mechanism,
        images,
        perf: synthetic_perf(),
        pinning: synthetic_pinning(),
        condition: "pinned-solo".to_string(),
        weights: Some(synthetic_weights()),
        skid_margin: Some(SYNTHETIC_SKID_MARGIN),
        attempted: records.len() as u64,
        planned,
        records_file: "records.jsonl".to_string(),
        records_sha256: sha,
    }
}

fn synth_sha256_of_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_lower(&h.finalize())
}

fn stage_slug(stage: Stage) -> &'static str {
    match stage {
        Stage::Aa0 => "aa0",
        Stage::Aa1 => "aa1",
        Stage::Aa2 => "aa2",
        Stage::Aa3 => "aa3",
        Stage::Aa4 => "aa4",
        Stage::Aa5 => "aa5",
        Stage::Aa6 => "aa6",
    }
}

/// Serialise a (manifest, records) pair into a [`Fixture`].
fn fixture(name: &'static str, run_set: &RunSet, records: &[RunRecord]) -> Fixture {
    Fixture {
        name,
        // Pretty-printed: the manifest is committed evidence a human reads. It is
        // not itself hashed by anything, so formatting is free to be legible.
        manifest_json: serde_json::to_string_pretty(run_set).expect("manifest JSON is infallible"),
        records_jsonl: records_jsonl(records),
    }
}

/// The valid AA-3 accept fixture: patched mechanism, exact landings, oracle-exact
/// counts, AND each input **repeated bit-identically** — because AA-3's acceptance
/// includes replay-identical landed-state digests, which a single-rep run cannot
/// prove. Everything a real AA-3 landing-run's evidence should be, in miniature.
fn accept() -> Fixture {
    // Two reps of each windowed payload, the pair sharing one landed_digest (a
    // bit-identical replay). Distinct only by sample_id, which is not in the RepKey.
    let mut records = Vec::new();
    for (i, &p) in WINDOWED.iter().enumerate() {
        for rep in 0..2u64 {
            let mut r = generate_record(
                i as u64 + rep * WINDOWED.len() as u64,
                p,
                ExitReason::Preempt,
            );
            // Same input ⇒ one landed state ⇒ one digest across the pair.
            if let Some(o) = r.overflow.as_mut() {
                o.landed_digest =
                    format!("sha256:{}", synth_sha256(&format!("landed-{}", p.name())));
            }
            r.state_digest = format!("sha256:{}", synth_sha256(&format!("state-{}", p.name())));
            records.push(r);
        }
    }
    let run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
    fixture("accept", &run_set, &records)
}

/// The valid AA-1 **counting-mode** accept fixture: no overflow armed, so every
/// record legitimately ends at the console sentinel with `ExitReason::Mmio`. The
/// checker used to reject this by comparing every record's exit reason against the
/// manifest's `expected_exit_reason` (which describes the armed landing) — so a
/// count-only run, the whole of AA-1(b), could not pass. It must now be accepted.
fn accept_counting() -> Fixture {
    let mut records = base_records(ExitReason::Mmio);
    for r in &mut records {
        // Counting mode: nothing armed, no landing, no sampling period.
        r.overflow = None;
    }
    let mut run_set = build_run_set(Stage::Aa1, stock_mechanism(), &records);
    // A counting run's perf event carries no sampling period.
    run_set.perf.sample_period = None;
    run_set.records_sha256 = synth_sha256_of_bytes(records_jsonl(&records).as_bytes());
    fixture("accept-counting", &run_set, &records)
}

/// The valid AA-1(c) skid-distribution accept fixture: a stock signal-kick run whose
/// landings scatter EARLY AND LATE around the target. AA-1(c) measures this
/// distribution to derive the margin, so a `target + k` landing is the datum, not a
/// violation — the checker must accept it. This is the counterpart to
/// `reject-overshoot`: the same positive skid that AA-4 forbids, AA-1 collects.
fn accept_aa1_skid() -> Fixture {
    let mut records = base_records(ExitReason::SignalKick);
    // Scatter the landings: some late (positive skid), some early (negative), all
    // self-consistent (skid == landed - target). None is a violation at AA-1.
    for (i, r) in records.iter_mut().enumerate() {
        if let Some(o) = r.overflow.as_mut() {
            let k = (i as i64 % 5) - 2; // -2, -1, 0, +1, +2, …
            o.landed = (o.target as i64 + k) as u64;
            o.skid = k;
        }
    }
    // AA-1 is still deriving the margin, so the manifest legitimately carries none.
    let mut run_set = build_run_set(Stage::Aa1, stock_mechanism(), &records);
    run_set.skid_margin = None;
    run_set.records_sha256 = synth_sha256_of_bytes(records_jsonl(&records).as_bytes());
    fixture("accept-aa1-skid", &run_set, &records)
}

/// The number of same-input repetitions the AA-6 fixtures use. Small — the real gate
/// floor is 1,000, but the fixtures test the checker's *logic*, not the number.
const AA6_REPS: u64 = 4;

/// `AA6_REPS` bit-identical repetitions of one payload's input: same payload, scale,
/// seed, condition, target, AND — within the payload — the same `landed_digest` and
/// `state_digest`. Distinct only by `sample_id`, which does not enter the [`RepKey`].
/// Each payload gets its own digests, so the reps form one group per payload.
fn aa6_reps_of(payload: Payload, first_id: u64) -> Vec<RunRecord> {
    (0..AA6_REPS)
        .map(|k| {
            let mut r = generate_record(first_id + k, payload, ExitReason::Preempt);
            // Bit-identical landings: every repetition of one input lands on one state,
            // so the whole group shares a single digest (what replay-identity compares).
            let name = payload.name();
            if let Some(o) = r.overflow.as_mut() {
                o.landed_digest = format!("sha256:{}", synth_sha256(&format!("aa6-landed-{name}")));
            }
            r.state_digest = format!("sha256:{}", synth_sha256(&format!("aa6-final-{name}")));
            r
        })
        .collect()
}

/// The valid AA-6 mini-gate accept fixture: the FULL determinism matrix — every
/// windowed payload, each repeated [`AA6_REPS`] times bit-identically. A run of one
/// payload repeated N times would satisfy the per-input rep floor yet fail the matrix
/// check; a real AA-6 gate covers the whole matrix, and this is that in miniature.
fn accept_aa6_gate() -> Fixture {
    // The full matrix: every windowed payload PLUS the AA-5 Linux guest.
    let mut records = Vec::new();
    for &p in WINDOWED.iter().chain(std::iter::once(&Payload::LinuxGuest)) {
        let first = records.len() as u64;
        records.extend(aa6_reps_of(p, first));
    }
    let run_set = build_run_set(Stage::Aa6, patched_mechanism(), &records);
    fixture("accept-aa6-gate", &run_set, &records)
}

/// The AA-2 step matrix: one row per transition class, each `(transition, pc_before, pc_after,
/// br_retired_delta)`. The values satisfy `check_debug_evidence`'s per-class rules — a
/// sequential step lands at exactly `pc_before + 4` with delta 0, a taken branch moves
/// `BR_RETIRED` by 1, a NOT-taken branch lands at `pc_before + 4` but still moves `BR_RETIRED`
/// by 1 (the branch instruction retired, AA1-F1), an LL/SC exclusive by 0, the
/// exception/WFI/injection classes by a bounded 0. Each is stepped twice (two reps of one step
/// position) so AA-2's replay identity has a repeated group to compare.
fn aa2_matrix() -> [(StepTransition, u64, u64, u64); 8] {
    let b = 0x4000_8000u64;
    [
        (StepTransition::Sequential, b, b + 4, 0),
        (StepTransition::TakenBranch, b + 0x100, b + 0x140, 1),
        (StepTransition::ExceptionEntry, b + 0x200, b + 0x240, 0),
        (StepTransition::ExceptionReturn, b + 0x300, b + 0x340, 0),
        (StepTransition::Wfi, b + 0x400, b + 0x404, 0),
        (StepTransition::Injection, b + 0x500, b + 0x540, 0),
        (StepTransition::LlscExclusive, b + 0x600, b + 0x604, 0),
        // A not-taken conditional: fell through to pc_before + 4, but the branch retired (delta 1).
        (StepTransition::NotTakenBranch, b + 0x700, b + 0x704, 1),
    ]
}

/// The AA-2 stepped record set: the full transition matrix, each class stepped twice
/// bit-identically. Every record is a StraightLine-at-smoke run (so it carries that run's
/// oracle-exact window count, exactly as a real stepped run does), overflow-free, on
/// `ExitReason::Debug`, with a valid single step of its class. The two reps of one step position
/// share its `step_index`, the step digest AND the final digest — a bit-identical replay. Modelled
/// as two runs, each stepping the matrix in order, so the two reps of class N are step N of each
/// run and share `step_index == N`.
fn aa2_records() -> Vec<RunRecord> {
    let mut records = Vec::new();
    for (step_index, (transition, pc_before, pc_after, delta)) in
        aa2_matrix().into_iter().enumerate()
    {
        // Two reps of one step position (same step_index, so one RepKey group).
        for rep in 0..2u64 {
            let id = records.len() as u64;
            let mut r = generate_record(id, Payload::StraightLine, ExitReason::Debug);
            // A stepped record is never an armed landing — mutually exclusive.
            r.overflow = None;
            let tag = format!("{transition:?}");
            r.step = Some(StepRecord {
                planned_sample_id: rep,
                // The within-run position; both reps of this class share it, so replay identity
                // compares step N of rep 1 to step N of rep 2.
                step_index: step_index as u64,
                pc_before,
                pc_after,
                // A single step retires exactly one instruction by construction.
                insn_retired: 1,
                br_retired_delta: delta,
                transition,
                // The step digest replay identity compares; shared within the rep group.
                step_digest: format!("sha256:{}", synth_sha256(&format!("aa2-step-{tag}"))),
            });
            // The final (sentinel) digest, shared within the rep group too.
            r.state_digest = format!("sha256:{}", synth_sha256(&format!("aa2-final-{tag}")));
            records.push(r);
        }
    }
    records
}

/// Turn an AA-2 stepped record set into a run-set: stage AA-2, the pre-patch stock mechanism
/// (AA-2 legitimately runs pre-patch), no armed sampling period (stepping arms guest debug, not
/// an overflow), weights present (so the oracle grades the window count each step carries).
fn aa2_run_set(records: &[RunRecord]) -> RunSet {
    let mut run_set = build_run_set(Stage::Aa2, stock_mechanism(), records);
    // A stepped run arms no overflow, so its perf event carries no sampling period.
    run_set.perf.sample_period = None;
    run_set.records_sha256 = synth_sha256_of_bytes(records_jsonl(records).as_bytes());
    run_set
}

/// The valid AA-2 accept fixture: the FULL single-step transition matrix, each class stepped
/// twice bit-identically. Every step is a valid single step (PC advanced, exactly one
/// instruction retired, `BR_RETIRED` delta consistent with the class), the matrix is complete
/// (all eight classes, including the LL/SC exclusive AA-4 needs), and each step position replayed
/// identically — everything AA-2's floor requires, in miniature. Today no run EMITS steps (the
/// stepping run path is arrival-day), so this is the shape a real AA-2 run-set will take.
fn accept_aa2_steps() -> Fixture {
    let records = aa2_records();
    let run_set = aa2_run_set(&records);
    fixture("accept-aa2-steps", &run_set, &records)
}

/// The valid AA-2 **bounded** accept fixture: the full transition matrix, each class stepped
/// twice bit-identically — but cut short at `--max-steps` before MARK_END, so every record's
/// window is `0/0/0` (a run that never closed its window; the shape a bounded llsc-livelock run
/// takes). Its window count is therefore NOT the oracle's, which is exactly the case
/// `check_counts` must EXEMPT: a step record is graded by debug-evidence / replay-identity, not
/// the window-count oracle. Grading it would reject a legitimately bounded run. The window
/// endpoints stay self-consistent (`measured_taken == work_end - work_begin`, enforced by
/// well-formed), and the step evidence and replay identity are unchanged from the unbounded
/// matrix — so the whole run-set is a clean AA-2 acceptance.
fn aa2_bounded_records() -> Vec<RunRecord> {
    let mut records = aa2_records();
    for r in &mut records {
        // Bounded: the window never closed, so the work fields are 0/0/0 — self-consistent but
        // decoupled from the oracle count the unbounded matrix carries.
        r.work_begin = 0;
        r.work_end = 0;
        r.measured_taken = 0;
    }
    records
}

fn accept_aa2_bounded() -> Fixture {
    let records = aa2_bounded_records();
    let run_set = aa2_run_set(&records);
    fixture("accept-aa2-bounded", &run_set, &records)
}

/// Build an AA-2 reject fixture by mutating ONE step of the otherwise-valid matrix, so
/// `check_debug_evidence` is the sole failure (the mutation touches neither the window count nor
/// the RepKey `step_index`/step digest, so counts and replay identity still pass — the mutated
/// rep still groups with its sibling rep on the same `step_index`, sharing its digest).
fn mutated_aa2_reject(name: &'static str, mutate: impl FnOnce(&mut Vec<RunRecord>)) -> Fixture {
    let mut records = aa2_records();
    mutate(&mut records);
    let run_set = aa2_run_set(&records);
    fixture(name, &run_set, &records)
}

/// The AA-6 rep-floor evasion, rejected: an eight-payload matrix of DISTINCT inputs
/// whose TOTAL record count meets a floor, though no single input is repeated even
/// twice. A total-count floor passed this; the per-input floor does not — AA-6 needs
/// N reps of the SAME input, not N distinct inputs once each.
fn reject_aa6_rep_floor() -> Fixture {
    // The eight windowed payloads (one rep each) PLUS a Linux-guest record, so the AA-6
    // MATRIX is complete and the sole remaining failure is the per-input rep floor — the
    // evasion this fixture isolates (many distinct inputs, none repeated).
    let mut records = base_records(ExitReason::Preempt);
    let mut lg = generate_record(
        records.len() as u64,
        Payload::LinuxGuest,
        ExitReason::Preempt,
    );
    lg.condition = "pinned-solo".to_string();
    records.push(lg);
    let run_set = build_run_set(Stage::Aa6, patched_mechanism(), &records);
    fixture("reject-aa6-rep-floor", &run_set, &records)
}

/// Generate all fixtures.
#[must_use]
pub fn all_fixtures() -> Vec<Fixture> {
    let mut fixtures = vec![
        accept(),
        accept_counting(),
        accept_aa1_skid(),
        accept_aa6_gate(),
        accept_aa2_steps(),
        accept_aa2_bounded(),
        reject_aa6_rep_floor(),
    ];

    // 1. reject-short-count — a valid but small run-set. The checker rejects it
    //    only when a floor larger than its armed-overflow count is demanded; the
    //    fixture itself is clean, so the floor is the sole failure. The test runs
    //    it with `--min-armed-overflows` above its count.
    {
        let records: Vec<RunRecord> = base_records(ExitReason::Preempt)
            .into_iter()
            .take(2)
            .collect();
        let run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        fixtures.push(fixture("reject-short-count", &run_set, &records));
    }

    // 2. reject-missing-sample — sample id 3 removed, `attempted` unchanged: a gap.
    {
        let mut records = base_records(ExitReason::Preempt);
        records.retain(|r| r.sample_id != 3);
        // `attempted` still reflects the full eight, so the gap is a totality fail.
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        run_set.attempted = 8;
        run_set.records_sha256 = synth_sha256_of_bytes(records_jsonl(&records).as_bytes());
        fixtures.push(fixture("reject-missing-sample", &run_set, &records));
    }

    // 3. reject-duplicate-overflow — one record delivered twice.
    fixtures.push(mutated_reject(
        "reject-duplicate-overflow",
        Stage::Aa3,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            if let Some(o) = records[0].overflow.as_mut() {
                o.deliveries = 2;
            }
        },
    ));

    // 4. reject-lost-pmi — one armed overflow never delivered.
    fixtures.push(mutated_reject(
        "reject-lost-pmi",
        Stage::Aa3,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            if let Some(o) = records[0].overflow.as_mut() {
                o.deliveries = 0;
            }
        },
    ));

    // 5. reject-count-mismatch — a WINDOW count that disagrees with the oracle. Kept
    //    self-consistent with its own window endpoints (measured_taken ==
    //    work_end - work_begin), and the overflow deadline is left alone (it is
    //    decoupled from the window count and carries a uniform period), so the *oracle*
    //    mismatch is the sole failure.
    fixtures.push(mutated_reject(
        "reject-count-mismatch",
        Stage::Aa3,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            records[0].measured_taken += 1;
            records[0].work_end += 1;
        },
    ));

    // 6. reject-overshoot — a landing PAST the target. Placed at AA-4, a patched
    //    LANDING-CONTRACT stage where the late-only-stop bound binds but exact landing
    //    is NOT required (that is AA-3's alone). A +1 skid within the margin therefore
    //    fails the overshoot sub-check ALONE. (Overshoot is valid evidence at AA-1(c),
    //    which measures the skid distribution — see the accept-aa1-skid fixture.)
    fixtures.push(mutated_reject(
        "reject-overshoot",
        Stage::Aa4,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            if let Some(o) = records[0].overflow.as_mut() {
                o.landed = o.target + 1;
                o.skid = 1;
            }
        },
    ));

    // 7. reject-skid-exceeds-margin — an early landing beyond the margin. Negative
    //    skid, so not an overshoot; also at AA-4 (bounds the margin, does not require
    //    exact), so the margin bound is the sole failure.
    fixtures.push(mutated_reject(
        "reject-skid-exceeds-margin",
        Stage::Aa4,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            if let Some(o) = records[0].overflow.as_mut() {
                let delta = SYNTHETIC_SKID_MARGIN + 1;
                o.landed = o.target - delta;
                o.skid = -(delta as i64);
            }
        },
    ));

    // 8. reject-stock-mechanism — the PR-98 failure: the manifest claims the
    //    patched Preempt exit, but every record carries the stock SignalKick.
    fixtures.push(mutated_reject(
        "reject-stock-mechanism",
        Stage::Aa3,
        patched_mechanism(),
        ExitReason::SignalKick,
        |_records| {},
    ));

    // 9. reject-unverified-image — a boot artifact pinned but never verified.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        if let Some(img) = run_set.images.get_mut(1) {
            img.verified_before_boot = false;
        }
        fixtures.push(fixture("reject-unverified-image", &run_set, &records));
    }

    // 10. reject-no-weights — the manifest carries no measured weights.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        run_set.weights = None;
        fixtures.push(fixture("reject-no-weights", &run_set, &records));
    }

    // 11. reject-tampered-records — records untouched, but the manifest pins the
    //     wrong sha256, as if the records file had been swapped after the fact.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        run_set.records_sha256 = "0".repeat(64);
        fixtures.push(fixture("reject-tampered-records", &run_set, &records));
    }

    // 12. reject-self-seeded-params — a record whose guest never saw the managed
    //     params page, so it silently ran the smoke scale.
    fixtures.push(mutated_reject(
        "reject-self-seeded-params",
        Stage::Aa3,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            records[0].params_mode = "self-seeded".to_string();
        },
    ));

    // 13. reject-aa3-claims-stock — the most PR-98-shaped evasion the checker exists
    //     to stop, and the one a per-record exit-reason check alone cannot see: an
    //     AA-3 run-set that declares the STOCK mechanism (kvm_patched: false,
    //     signal-kick) and whose records all carry signal-kick. Everything agrees
    //     with everything; what they agree on is AA-3's forbidden fallback. Stage
    //     AA-3 rides the patched force-exit, so the tuple — not the consistency — is
    //     what decides.
    {
        let records = base_records(ExitReason::SignalKick);
        let run_set = build_run_set(Stage::Aa3, stock_mechanism(), &records);
        fixtures.push(fixture("reject-aa3-claims-stock", &run_set, &records));
    }

    // 14. reject-migration-probe-outside-aa1 — an unpinned AA-3 landing run that
    //     exempts itself from pinning by setting one manifest field. The bounded
    //     migration probe is AA-1's alone (docs/ARM-ALTRA.md §AA-1); pinning is a
    //     correctness condition on this lineage (rr #3607), not hygiene.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        run_set.pinning.pinned = false;
        run_set.pinning.migration_probe = true;
        run_set.pinning.core = None;
        fixtures.push(fixture(
            "reject-migration-probe-outside-aa1",
            &run_set,
            &records,
        ));
    }

    // 15. reject-perf-attrs — the manifest's perf block records an event that is not
    //     the work clock: a different raw event, host-inclusive, guest-EXCLUDING, and
    //     unpinned (so multiplexed, so scaled). Evidence that cannot establish what
    //     the run-set claims, and which nothing used to check.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        run_set.perf = PerfConfig {
            raw_event: 0,
            exclude_host: false,
            exclude_guest: true,
            exclude_hv: false,
            pinned: false,
            sample_period: Some(1_000_000),
        };
        fixtures.push(fixture("reject-perf-attrs", &run_set, &records));
    }

    // 16. reject-clockpage-self-seeded — an AA-5 run whose guests all published their
    //     OWN static clock page, because the harness never published one. That run
    //     exercised the fallback, not the harness-maintained work-derived page AA-5
    //     exists to certify.
    {
        let mut records = base_records(ExitReason::Preempt);
        for r in &mut records {
            r.clockpage_mode = Some("self-seeded".to_string());
        }
        let run_set = build_run_set(Stage::Aa5, patched_mechanism(), &records);
        fixtures.push(fixture("reject-clockpage-self-seeded", &run_set, &records));
    }

    // 16b. accept-aa1-llsc-hazard — at AA-1, two same-seed llsc-atomics repetitions
    //     with DIFFERENT digests (and self-consistent counts differing by their own
    //     reported retries) are the §4 hazard, measured — recorded on the verdict,
    //     never failed. Observed spontaneously on harmony-arm (AA1-F2): a host IRQ
    //     between LDXR and STXR clears the monitor. Any OTHER payload diverging, or
    //     llsc at a later stage, still fails (fixture 17).
    {
        // Counting-mode records, as observed live (overflow: null, exit mmio) — the
        // divergence appeared with NO armed overflow and NO injection.
        let mut a = generate_record(0, Payload::LlscAtomics, ExitReason::Mmio);
        let mut b = generate_record(1, Payload::LlscAtomics, ExitReason::Mmio);
        a.overflow = None;
        b.overflow = None;
        b.reported_taken = a.reported_taken + 1;
        b.work_end += 1; // the retry's extra CBNZ execution...
        b.measured_taken += 1; // ...so the count stays exact for ITS OWN retry term
        a.state_digest = format!("sha256:{}", synth_sha256("llsc-rep-a"));
        b.state_digest = format!("sha256:{}", synth_sha256("llsc-rep-b"));
        let records = vec![a, b];
        let mut run_set = build_run_set(Stage::Aa1, stock_mechanism(), &records);
        // A counting run: no uniform sampling period in the manifest.
        run_set.perf.sample_period = None;
        fixtures.push(fixture("accept-aa1-llsc-hazard", &run_set, &records));
    }

    // 17. reject-divergent-digests — two repetitions of the SAME (payload, scale,
    //     seed, condition, target) that landed on different state digests. Every
    //     count matches, every overflow was delivered exactly once, the sample count
    //     meets any rep floor you like — and the two runs did different things. This
    //     is the vacuity the rep floor had: it counted rows and never compared them.
    {
        let mut a = generate_record(0, Payload::StraightLine, ExitReason::Preempt);
        let mut b = generate_record(1, Payload::StraightLine, ExitReason::Preempt);
        a.state_digest = format!("sha256:{}", synth_sha256("state-rep-a"));
        b.state_digest = format!("sha256:{}", synth_sha256("state-rep-b"));
        let records = vec![a, b];
        let run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        fixtures.push(fixture("reject-divergent-digests", &run_set, &records));
    }

    // 18. reject-pinned-no-core — pinned: true with core: null. The recorded core is
    //     required evidence for the rr #3607 migration condition; the schema itself
    //     describes this tuple as unverifiable.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        run_set.pinning.pinned = true;
        run_set.pinning.core = None;
        fixtures.push(fixture("reject-pinned-no-core", &run_set, &records));
    }

    // 19. reject-malformed-hash — an image sha256 that serde accepts (it is a String)
    //     but the schema's `^[0-9a-f]{64}$` pattern does not. Serde's type check is not
    //     the schema's constraint check; the well-formed gate is.
    {
        let records = base_records(ExitReason::Preempt);
        let mut run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
        // Corrupt a PAYLOAD image's hash (not the kernel's, which the image-pin cross-check
        // would also flag): the well-formed gate must catch a schema-invalid hash on its own.
        if let Some(img) = run_set.images.get_mut(1) {
            img.sha256 = String::new(); // empty: not 64 hex
        }
        fixtures.push(fixture("reject-malformed-hash", &run_set, &records));
    }

    // 20. reject-aa2-step-skips-insn — a SEQUENTIAL step that advanced by 8, not 4: an
    //     instruction was skipped. AArch64 instructions are a fixed 4 bytes, so a sequential
    //     step must land at EXACTLY pc_before + 4; a larger jump is the miss AA-2 exists to
    //     catch. One rep of the sequential step-moment is mutated, so the RepKey/step-moment
    //     digest is untouched and replay identity still passes — debug-evidence is the sole
    //     failure.
    fixtures.push(mutated_aa2_reject(
        "reject-aa2-step-skips-insn",
        |records| {
            if let Some(s) = records[0].step.as_mut() {
                s.pc_after = s.pc_before + 8;
            }
        },
    ));

    // 21. reject-aa2-step-doubled — a step that retired TWO instructions, not the exactly one
    //     AA-2's single-step semantics require. The window count and the step-moment are
    //     untouched, so debug-evidence alone fails.
    fixtures.push(mutated_aa2_reject("reject-aa2-step-doubled", |records| {
        if let Some(s) = records[0].step.as_mut() {
            s.insn_retired = 2;
        }
    }));

    // 22. reject-aa2-taken-branch-no-branch — a step CLASSIFIED as a taken branch whose
    //     BR_RETIRED did not move (delta 0). A taken branch must increment BR_RETIRED by
    //     exactly 1; this disagreement between the opcode's class and the measured counter is
    //     the AA-2 finding the checker surfaces. `records[2]` is the first taken-branch rep.
    fixtures.push(mutated_aa2_reject(
        "reject-aa2-taken-branch-no-branch",
        |records| {
            if let Some(s) = records[2].step.as_mut() {
                s.br_retired_delta = 0;
            }
        },
    ));

    // 23. reject-aa2-dropped-planned-sample — the single-step TOTALITY defect (J2). Two planned
    //     runs each emitted eight steps, but an attacker duplicates planned id 0 over id 1. The
    //     file still has two `step_index == 0` rows and dense record ids, so the bounced counter
    //     check passed it; distinct stable planned ids expose id 1 as missing.
    {
        let original = aa2_records();
        let mut run_set = aa2_run_set(&original);
        let mut records = original;
        for record in &mut records {
            if let Some(step) = record.step.as_mut() {
                step.planned_sample_id = 0;
            }
        }
        run_set.records_sha256 = synth_sha256_of_bytes(records_jsonl(&records).as_bytes());
        fixtures.push(fixture(
            "reject-aa2-dropped-planned-sample",
            &run_set,
            &records,
        ));
    }

    fixtures
}

/// Build a reject fixture by generating the base records for `exit`, applying a
/// mutation, and re-pinning the records' sha (so only the mutation, not a stale
/// hash, is what fails — except where the hash *is* the point).
fn mutated_reject(
    name: &'static str,
    stage: Stage,
    mechanism: Mechanism,
    exit: ExitReason,
    mutate: impl FnOnce(&mut Vec<RunRecord>),
) -> Fixture {
    let mut records = base_records(exit);
    mutate(&mut records);
    let run_set = build_run_set(stage, mechanism, &records);
    fixture(name, &run_set, &records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_thirty_one_fixtures_with_unique_names() {
        let fixtures = all_fixtures();
        assert_eq!(fixtures.len(), 31);
        let mut names: Vec<&str> = fixtures.iter().map(|f| f.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 31, "fixture names must be unique");
    }

    #[test]
    fn the_aa2_accept_matrix_covers_every_transition_class_twice() {
        // The generator's own invariant: the AA-2 accept fixture steps all eight transition
        // classes, each twice (a repeated step position for replay identity), on Debug exits with
        // no armed overflow.
        let records = aa2_records();
        assert_eq!(records.len(), 16, "eight classes × two reps");
        let mut classes: Vec<StepTransition> = records
            .iter()
            .map(|r| r.step.as_ref().expect("stepped").transition)
            .collect();
        classes.sort_unstable();
        classes.dedup();
        assert_eq!(classes.len(), 8, "every transition class is covered");
        for r in &records {
            assert_eq!(r.exit_reason, ExitReason::Debug);
            assert!(r.overflow.is_none(), "a stepped record is never armed");
            let s = r.step.as_ref().expect("stepped");
            assert_eq!(s.insn_retired, 1);
            assert_ne!(s.pc_after, s.pc_before, "a step advances the PC");
        }
    }

    #[test]
    fn accept_manifest_pins_the_real_records_hash() {
        // The generator's own invariant: every non-tampered fixture pins the true
        // sha256 of its records. If this ever broke, the accept fixture would fail
        // the records-sha256 check for the wrong reason.
        let f = accept();
        let run_set: RunSet = serde_json::from_str(&f.manifest_json).expect("valid manifest");
        let real = synth_sha256_of_bytes(f.records_jsonl.as_bytes());
        assert_eq!(run_set.records_sha256, real);
    }

    #[test]
    fn accept_records_are_oracle_exact() {
        // The counts really are what the oracle predicts under the synthetic
        // weights — the fixtures are generated, not hand-tuned.
        let f = accept();
        for line in f.records_jsonl.lines() {
            let r: RunRecord = serde_json::from_str(line).expect("valid record");
            let e = expected(r.payload, r.scale, r.seed);
            assert_eq!(
                r.measured_taken,
                e.total(&synthetic_weights(), r.reported_taken)
                    .expect("synthetic weights do not overflow"),
                "payload {}",
                r.payload.name()
            );
            assert_eq!(r.measured_taken, r.work_end - r.work_begin);
        }
    }
}
