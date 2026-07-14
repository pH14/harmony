//! The fixture generator.
//!
//! Seventeen synthetic run-sets the checker must reject (one per failure mode) and
//! one it must accept. They are **generated from the oracle model**, not hand
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
    RunSet, SCHEMA_VERSION, Stage, hex_lower,
};
use oracle_model::{DEFAULT_SEED, Payload, Scale, Weights, expected};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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

/// The boot artifacts every fixture pins. All verified, in the accept case.
fn synthetic_images() -> Vec<ImagePin> {
    vec![
        ImagePin {
            path: "/boot/Image.det".to_string(),
            sha256: synth_sha256("host-kernel-image"),
            md5: synth_md5("host-kernel-image"),
            verified_before_boot: true,
        },
        ImagePin {
            path: "payloads/target/oracle.elf".to_string(),
            sha256: synth_sha256("payload-elf"),
            md5: synth_md5("payload-elf"),
            verified_before_boot: true,
        },
    ]
}

fn synthetic_perf() -> PerfConfig {
    PerfConfig {
        // 0x21 = BR_RETIRED (retired taken branches).
        raw_event: 0x21,
        exclude_host: true,
        exclude_guest: false,
        exclude_hv: true,
        pinned: true,
        sample_period: Some(1_000_000),
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
    let measured_taken = e.total(&synthetic_weights(), reported_taken);
    let work_begin = 1_000;
    let work_end = work_begin + measured_taken;
    // The token the guest actually prints when the harness published the page
    // (`payloads/runtime/src/pvclock.rs`): `managed`, versus `self-seeded` for the
    // payload's own static fallback. The AA-5 check reads this field, so a fixture
    // that invented a third token would be testing a string no guest can emit.
    let clockpage_mode = if payload == Payload::ClockPage {
        Some("managed".to_string())
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
            target: measured_taken,
            landed: measured_taken,
            skid: 0,
        }),
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
    RunSet {
        schema_version: SCHEMA_VERSION,
        stage,
        run_set_id: format!("fixture-{}", stage_slug(stage)),
        environment: synthetic_environment(),
        mechanism,
        images: synthetic_images(),
        perf: synthetic_perf(),
        pinning: synthetic_pinning(),
        condition: "pinned-solo".to_string(),
        weights: Some(synthetic_weights()),
        skid_margin: Some(SYNTHETIC_SKID_MARGIN),
        attempted: records.len() as u64,
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
/// counts. Everything a real AA-3 landing-run's evidence should be, in miniature.
fn accept() -> Fixture {
    let records = base_records(ExitReason::Preempt);
    let run_set = build_run_set(Stage::Aa3, patched_mechanism(), &records);
    fixture("accept", &run_set, &records)
}

/// Generate all eighteen fixtures.
#[must_use]
pub fn all_fixtures() -> Vec<Fixture> {
    let mut fixtures = vec![accept()];

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

    // 5. reject-count-mismatch — a count that disagrees with the oracle. Kept
    //    self-consistent with its own window endpoints (measured_taken ==
    //    work_end - work_begin), so the *oracle* mismatch is the sole failure.
    fixtures.push(mutated_reject(
        "reject-count-mismatch",
        Stage::Aa3,
        patched_mechanism(),
        ExitReason::Preempt,
        |records| {
            records[0].measured_taken += 1;
            records[0].work_end += 1;
            if let Some(o) = records[0].overflow.as_mut() {
                o.target += 1;
                o.landed += 1;
            }
        },
    ));

    // 6. reject-overshoot — a landing past the target. AA-1 (skid-distribution)
    //    stage, so exact landing is not required and overshoot is the sole failure.
    fixtures.push(mutated_reject(
        "reject-overshoot",
        Stage::Aa1,
        stock_mechanism(),
        ExitReason::SignalKick,
        |records| {
            if let Some(o) = records[0].overflow.as_mut() {
                o.landed = o.target + 1;
                o.skid = 1;
            }
        },
    ));

    // 7. reject-skid-exceeds-margin — an early landing beyond the margin. Negative
    //    skid, so it is not an overshoot; the margin bound is the sole failure.
    fixtures.push(mutated_reject(
        "reject-skid-exceeds-margin",
        Stage::Aa1,
        stock_mechanism(),
        ExitReason::SignalKick,
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
    fn there_are_eighteen_fixtures_with_unique_names() {
        let fixtures = all_fixtures();
        assert_eq!(fixtures.len(), 18);
        let mut names: Vec<&str> = fixtures.iter().map(|f| f.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 18, "fixture names must be unique");
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
                e.total(&synthetic_weights(), r.reported_taken),
                "payload {}",
                r.payload.name()
            );
            assert_eq!(r.measured_taken, r.work_end - r.work_begin);
        }
    }
}
