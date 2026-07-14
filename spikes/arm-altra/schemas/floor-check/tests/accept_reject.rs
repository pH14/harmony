// SPDX-License-Identifier: AGPL-3.0-or-later
// Every test here reads a committed fixture off disk, and Miri's isolation forbids
// `open`. The crate has no `unsafe`, so the interpreter has nothing to say about it
// anyway — and disabling isolation to satisfy a file-reading test is exactly the
// wrong trade (the repo's Miri jobs run with `-Zmiri-permissive-provenance` only).
// So the target steps aside under Miri rather than the flag being loosened for it.
#![cfg(not(miri))]

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
    // Checked with the armed-overflow floor it meets. No --min-reps: the accept
    // fixture is an AA-3 landing run of eight DISTINCT inputs (one rep each), not an
    // AA-6 same-input gate, so a per-input rep floor above 1 does not apply to it —
    // the AA-6 gate is covered by accept-aa6-gate.
    let floors = Floors {
        min_armed_overflows: Some(8),
        min_reps: None,
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
fn accept_counting_is_accepted() {
    // AA-1(b): a count-only run whose records end on ExitReason::Mmio (no overflow
    // armed). The checker used to reject every such run by comparing the unarmed
    // exit against the manifest's expected mechanism; it must now accept it. No
    // armed-overflow floor is requested because nothing armed one.
    let report = check("accept-counting", no_floors());
    assert!(
        report.passed(),
        "the counting-mode fixture was rejected: {:?}",
        report.failed()
    );
    assert_eq!(report.exit_code(), 0);
}

#[test]
fn accept_aa6_gate_is_accepted() {
    // A real AA-6 mini-gate shape: the SAME input repeated, all bit-identical. The
    // per-input rep floor accepts it (four reps of one input meets a floor of four),
    // and replay-identity confirms the four landed identically.
    let floors = Floors {
        min_armed_overflows: Some(4),
        min_reps: Some(4),
    };
    let report = check("accept-aa6-gate", floors);
    assert!(
        report.passed(),
        "the AA-6 gate fixture was rejected: {:?}",
        report.failed()
    );
    assert_eq!(report.exit_code(), 0);
}

#[test]
fn reject_aa6_rep_floor_counts_per_input_not_total() {
    // The evasion the per-input floor closes: eight DISTINCT inputs, one rep each. The
    // total (8) meets a floor of 2, but no input is repeated even twice — which a
    // total-count floor accepted and the per-input floor rejects.
    let floors = Floors {
        min_armed_overflows: Some(8),
        min_reps: Some(2),
    };
    assert_single_failure("reject-aa6-rep-floor", floors, CheckId::RepFloor);
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
    // AA-4 (a patched landing-contract stage): a +1 landing is forbidden. The floor
    // is passed so the armed overflows are accounted, leaving Skid the sole failure.
    let floors = Floors {
        min_armed_overflows: Some(1),
        min_reps: None,
    };
    assert_single_failure("reject-overshoot", floors, CheckId::Skid);
}

#[test]
fn reject_skid_exceeds_margin() {
    let floors = Floors {
        min_armed_overflows: Some(1),
        min_reps: None,
    };
    assert_single_failure("reject-skid-exceeds-margin", floors, CheckId::Skid);
}

#[test]
fn accept_aa1_skid_is_accepted_positive_skid_and_all() {
    // The counterpart to reject-overshoot: AA-1(c) MEASURES the early/late skid
    // distribution, so a landing at target+k is the datum, not a violation. The same
    // positive skid AA-4 forbids, AA-1 must accept. `skid_margin: null` is legitimate
    // here — the stage is deriving it.
    let floors = Floors {
        min_armed_overflows: Some(8),
        min_reps: None,
    };
    let report = check("accept-aa1-skid", floors);
    assert!(
        report.passed(),
        "the AA-1 skid-distribution fixture was rejected: {:?}",
        report.failed()
    );
    assert_eq!(report.exit_code(), 0);
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

#[test]
fn reject_aa3_claims_stock() {
    // The evasion a per-record exit-reason check cannot see: an AA-3 run-set that
    // declares the stock mechanism AND whose records all carry the stock signal-kick.
    // Everything agrees with everything; what they agree on is AA-3's forbidden
    // fallback. The stage tuple is what refuses it — self-consistency is not
    // attestation.
    assert_single_failure(
        "reject-aa3-claims-stock",
        no_floors(),
        CheckId::MechanismAttestation,
    );
}

#[test]
fn reject_migration_probe_outside_aa1() {
    // One manifest field must not exempt an unpinned AA-3 landing run from a
    // correctness condition (rr #3607). The bounded probe is AA-1's alone.
    assert_single_failure(
        "reject-migration-probe-outside-aa1",
        no_floors(),
        CheckId::Pinning,
    );
}

#[test]
fn reject_perf_attrs() {
    // A run-set whose perf block records an event that is not the work clock:
    // wrong raw event, host-inclusive, guest-EXCLUDING, unpinned (so multiplexed,
    // so scaled). Such evidence cannot establish what the run-set claims.
    assert_single_failure("reject-perf-attrs", no_floors(), CheckId::PerfConfig);
}

#[test]
fn reject_clockpage_self_seeded() {
    // An AA-5 run whose guests published their own static clock page: it exercised
    // the fallback, not the harness-maintained work-derived page AA-5 certifies.
    assert_single_failure(
        "reject-clockpage-self-seeded",
        no_floors(),
        CheckId::ClockPageMode,
    );
}

#[test]
fn reject_divergent_digests() {
    // Two repetitions of the same input that landed on different states. Every count
    // matches, every overflow was delivered exactly once, and any rep floor you care
    // to name is met — because a rep floor counts rows. This is the axis it exists
    // for, and the replay-identity check is what actually reads it.
    let floors = Floors {
        min_armed_overflows: None,
        min_reps: Some(2),
    };
    assert_single_failure("reject-divergent-digests", floors, CheckId::ReplayIdentity);
}

#[test]
fn an_unrequested_floor_is_not_an_accepted_one() {
    // §Evidence integrity #2: the checker's output IS retained evidence. Checking an
    // overflow-bearing run-set with no --min-armed-overflows must not read as full
    // acceptance — the omission is on the face of the verdict, and the RC is nonzero.
    let report = check("accept", no_floors());
    assert!(
        !report.passed(),
        "a floor nobody asked for is not a floor that passed"
    );
    assert_eq!(report.failed(), Vec::new(), "nothing actually FAILED");
    assert_eq!(
        report.not_requested(),
        vec![CheckId::ArmedOverflowFloor],
        "the unrequested floor is named in the retained verdict"
    );
    assert_ne!(report.exit_code(), 0);
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
