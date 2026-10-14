// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;
use unison::DivergencePoint;
use unison::toy::generate_program;

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
fn toy_compare_matching_checkpoints_with_exit_code_0() {
    let (code, json) = run(&[
        "toy-compare",
        "--seed",
        "7",
        "--diverge-at",
        "18446744073709551615",
        "--checkpoint-every",
        "64",
        "--limit",
        "1000",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(json["verdict"], serde_json::json!("matching_checkpoints"));
    assert_eq!(json["checkpoint_every"], 64);
    assert_eq!(json["requested_limit"], 1000);
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

    let point: DivergencePoint =
        serde_json::from_value(json["point"].clone()).expect("point must deserialize");
    assert_eq!(serde_json::to_value(&point).unwrap(), json["point"]);

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
    assert_eq!(
        json["compare"]["verdict"],
        serde_json::json!("matching_checkpoints")
    );
    assert_eq!(json["compare"]["checkpoint_every"], 125);
    assert_eq!(json["compare"]["requested_limit"], 2000);
}

#[test]
fn toy_bisect_reports_a_spawn_difference_with_zero_limit() {
    let (code, json) = run(&[
        "toy-bisect",
        "--seed",
        "42",
        "--diverge-at",
        "0",
        "--limit",
        "0",
    ]);
    assert_eq!(code, Some(2));
    assert_eq!(json["compare"]["verdict"]["diverged"]["first_mismatch"], 0);
    assert_eq!(json["point"]["first_divergent_work"], 0);
}

#[test]
fn toy_bisect_recovers_a_replayable_terminal_point() {
    let halted_at = generate_program(0, 1).work_to_halt;
    let diverge_at = halted_at.to_string();
    let limit = halted_at.saturating_add(1).to_string();
    let (code, json) = run(&[
        "toy-bisect",
        "--seed",
        "7",
        "--diverge-at",
        &diverge_at,
        "--limit",
        &limit,
        "--program-seed",
        "0",
        "--min-work",
        "1",
    ]);
    assert_eq!(code, Some(2));
    assert_eq!(
        json["compare"]["verdict"]["terminal_diverged"]["halted_at"],
        halted_at
    );
    assert_eq!(json["point"]["first_divergent_work"], halted_at);
}

#[test]
fn toy_bisect_recovers_a_directly_replayable_point_before_halt() {
    let halted_at = generate_program(0, 1_000).work_to_halt;
    assert!(halted_at > 1);
    let diverge_at = halted_at.saturating_sub(1).to_string();
    let limit = halted_at.saturating_add(1).to_string();
    let (code, json) = run(&[
        "toy-bisect",
        "--seed",
        "7",
        "--diverge-at",
        &diverge_at,
        "--limit",
        &limit,
        "--program-seed",
        "0",
        "--min-work",
        "1000",
    ]);
    assert_eq!(code, Some(2));
    assert_eq!(
        json["compare"]["verdict"]["terminal_diverged"]["halted_at"],
        halted_at
    );
    assert_eq!(json["point"]["first_divergent_work"], halted_at - 1);
}
