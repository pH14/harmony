// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance gate 6: CLI smoke test — invoke both subcommands as real
//! processes, parse the JSON, and check the divergence point round-trips.

use std::process::Command;
use unison::DivergencePoint;

fn run(args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_unison"))
        .args(args)
        .output()
        .expect("failed to run unison binary");
    let stdout = String::from_utf8(out.stdout).expect("stdout must be UTF-8");
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not a single JSON object: {e}\n{stdout}"));
    (out.status.code(), json)
}

#[test]
fn toy_compare_detects_divergence_with_exit_code_2() {
    let (code, json) = run(&[
        "toy-compare",
        "--seed",
        "7",
        "--diverge-at",
        "500",
        "--checkpoint-every",
        "64",
        "--limit",
        "1000",
    ]);
    assert_eq!(code, Some(2));
    let diverged = &json["verdict"]["diverged"];
    let last_match = diverged["last_match"].as_u64().expect("last_match");
    let first_mismatch = diverged["first_mismatch"].as_u64().expect("first_mismatch");
    assert!(
        last_match < 500 && 500 <= first_mismatch,
        "bracket ({last_match}, {first_mismatch}] should contain 500"
    );
    assert_eq!(json["limit_reached"], serde_json::json!(false));
}

#[test]
fn toy_compare_identical_with_exit_code_0() {
    let (code, json) = run(&[
        "toy-compare",
        "--seed",
        "7",
        "--diverge-at",
        "18446744073709551615", // u64::MAX = never
        "--checkpoint-every",
        "64",
        "--limit",
        "1000",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(json["verdict"], serde_json::json!("identical"));
    // Identical-up-to-limit, NOT identical-forever: the caveat must surface.
    assert_eq!(json["limit_reached"], serde_json::json!(true));
}

#[test]
fn toy_bisect_point_round_trips() {
    let (code, json) = run(&[
        "toy-bisect",
        "--seed",
        "42",
        "--diverge-at",
        "777",
        "--limit",
        "2000",
    ]);
    assert_eq!(code, Some(2));

    // Typed round-trip: JSON -> DivergencePoint -> JSON is the identity...
    let point: DivergencePoint =
        serde_json::from_value(json["point"].clone()).expect("point must deserialize");
    assert_eq!(serde_json::to_value(&point).unwrap(), json["point"]);

    // ...and the point is the injected ground truth.
    assert_eq!(point.first_divergent_work, 777);
    assert_ne!(point.hash_a, point.hash_b);
    assert!(point.runs_executed > 0);
}

#[test]
fn toy_bisect_without_divergence_reports_null_point() {
    let (code, json) = run(&[
        "toy-bisect",
        "--seed",
        "42",
        "--diverge-at",
        "18446744073709551615",
        "--limit",
        "2000",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(json["point"], serde_json::Value::Null);
    assert_eq!(json["compare"]["verdict"], serde_json::json!("identical"));
}
