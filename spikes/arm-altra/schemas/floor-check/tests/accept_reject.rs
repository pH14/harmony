//! Accept/reject integration tests over the committed fixtures.
//!
//! Each test loads a checked-in fixture from `schemas/fixtures/<case>/` and runs
//! the real checker over it, asserting not merely that a reject fails but *which*
//! check catches it — the property that makes the fixtures a regression net rather
//! than a smoke test. A final drift test regenerates the fixtures from the model in
//! memory and asserts the committed files still match, so a model change that would
//! silently invalidate a fixture fails the build instead of rotting the evidence.

use std::path::PathBuf;

use floor_check::fixtures::all_fixtures;
use floor_check::{CheckId, CheckReport, Floors, Status, check_run_set};

/// `schemas/fixtures/`, resolved from this crate's manifest dir.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("fixtures")
}

fn check(name: &str, floors: Floors) -> CheckReport {
    let dir = fixtures_dir().join(name);
    check_run_set(&dir, &floors).unwrap_or_else(|e| panic!("loading fixture {name}: {e}"))
}

fn no_floors() -> Floors {
    Floors::default()
}

/// A reject fixture whose single failing check is `id`, checked under `floors`.
fn assert_single_failure(name: &str, floors: Floors, id: CheckId) {
    let report = check(name, floors);
    assert!(!report.passed(), "{name} was accepted but must be rejected");
    assert_eq!(
        report.status_of(id),
        Some(Status::Fail),
        "{name}: expected {id} to fail; report failed {:?}",
        report.failed()
    );
    assert_eq!(
        report.failed(),
        vec![id],
        "{name}: expected exactly one failing check ({id}), got {:?}",
        report.failed()
    );
}

#[test]
fn accept_is_accepted() {
    // Checked with the floors it meets: eight armed overflows, eight reps.
    let floors = Floors {
        min_armed_overflows: Some(8),
        min_reps: Some(8),
    };
    let report = check("accept", floors);
    assert!(
        report.passed(),
        "accept fixture was rejected: {:?}",
        report.failed()
    );
    assert_eq!(report.exit_code(), 0);
}

#[test]
fn reject_short_count() {
    // The fixture is valid; it fails only because a floor larger than its
    // armed-overflow count is demanded. That makes the floor the sole failure.
    let floors = Floors {
        min_armed_overflows: Some(1_000_000),
        min_reps: None,
    };
    assert_single_failure("reject-short-count", floors, CheckId::ArmedOverflowFloor);
}

#[test]
fn reject_missing_sample() {
    assert_single_failure("reject-missing-sample", no_floors(), CheckId::Totality);
}

#[test]
fn reject_duplicate_overflow() {
    assert_single_failure(
        "reject-duplicate-overflow",
        no_floors(),
        CheckId::Multiplicity,
    );
}

#[test]
fn reject_lost_pmi() {
    assert_single_failure("reject-lost-pmi", no_floors(), CheckId::Multiplicity);
}

#[test]
fn reject_count_mismatch() {
    assert_single_failure(
        "reject-count-mismatch",
        no_floors(),
        CheckId::CountExactness,
    );
}

#[test]
fn reject_overshoot() {
    assert_single_failure("reject-overshoot", no_floors(), CheckId::Skid);
}

#[test]
fn reject_skid_exceeds_margin() {
    assert_single_failure("reject-skid-exceeds-margin", no_floors(), CheckId::Skid);
}

#[test]
fn reject_stock_mechanism() {
    // The PR-98 lesson: a run that silently exercised the stock signal-kick path
    // while claiming the patched Preempt exit must fail here.
    assert_single_failure(
        "reject-stock-mechanism",
        no_floors(),
        CheckId::MechanismAttestation,
    );
}

#[test]
fn reject_unverified_image() {
    assert_single_failure("reject-unverified-image", no_floors(), CheckId::ImagePins);
}

#[test]
fn reject_no_weights() {
    // Two checks fail here — weights-present, and count-exactness which is refused
    // without them — so this is not a single-failure case. What matters is that the
    // checker refused to substitute a default: weights-present is the failure, and
    // counts were not silently graded against an invented weight.
    let report = check("reject-no-weights", no_floors());
    assert!(!report.passed());
    assert_eq!(
        report.status_of(CheckId::WeightsPresent),
        Some(Status::Fail)
    );
    assert_eq!(
        report.status_of(CheckId::CountExactness),
        Some(Status::Fail)
    );
}

#[test]
fn reject_tampered_records() {
    assert_single_failure(
        "reject-tampered-records",
        no_floors(),
        CheckId::RecordsSha256,
    );
}

#[test]
fn reject_self_seeded_params() {
    assert_single_failure(
        "reject-self-seeded-params",
        no_floors(),
        CheckId::ParamsMode,
    );
}

/// The committed fixture files must byte-match what the generator emits today. If
/// the oracle model or the evidence shapes change, this fails until `gen-fixtures`
/// is re-run — the fixtures can never silently drift from the model they test.
#[test]
fn fixtures_match_committed() {
    let dir = fixtures_dir();
    for f in all_fixtures() {
        let manifest = dir.join(f.name).join("run-set.json");
        let records = dir.join(f.name).join("records.jsonl");
        let committed_manifest = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("reading {}: {e}", manifest.display()));
        let committed_records = std::fs::read_to_string(&records)
            .unwrap_or_else(|e| panic!("reading {}: {e}", records.display()));
        assert_eq!(
            committed_manifest, f.manifest_json,
            "{}/run-set.json drifted from the generator; re-run `cargo run --bin gen-fixtures`",
            f.name
        );
        assert_eq!(
            committed_records, f.records_jsonl,
            "{}/records.jsonl drifted from the generator; re-run `cargo run --bin gen-fixtures`",
            f.name
        );
    }
}
