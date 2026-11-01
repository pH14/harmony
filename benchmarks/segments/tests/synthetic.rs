// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_segment-bench");

fn specs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/specs")
}

fn arms(dir: &Path) -> PathBuf {
    let arm = |factor: &str, key: &str| json!({"command": [BIN, "stub-adapter", "--factor", factor, "--stream-key", key]});
    let path = dir.join("arms.json");
    let value = json!({
        "control": arm("1.0", "control"),
        "faster": arm("0.4", "faster"),
        "slower": arm("2.5", "slower"),
        "same": arm("1.0", "same"),
    });
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    path
}

fn bench(args: &[&str]) -> (bool, String) {
    let output = Command::new(BIN).args(args).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn the_verdict_separates_faster_slower_and_same_arms() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let out = tmp.path().join("out");
    let (specs, arms, out) = (
        specs(),
        arms.to_str().unwrap().to_owned(),
        out.to_str().unwrap().to_owned(),
    );
    let specs = specs.to_str().unwrap();
    let run = [
        "run", "--specs", specs, "--arms", &arms, "--arm", "control", "--arm", "faster", "--arm",
        "slower", "--arm", "same", "--seeds", "1-64", "--jobs", "8", "--out", &out,
    ];
    let (ok, text) = bench(&run);
    assert!(ok, "{text}");
    assert!(
        text.contains("768 runs: 0 already have a result, 768 to run"),
        "{text}"
    );
    let (ok, text) = bench(&run);
    assert!(
        ok && text.contains("768 runs: 768 already have a result, 0 to run"),
        "{text}"
    );
    let (ok, text) = bench(&[
        "score",
        "--specs",
        specs,
        "--out",
        &out,
        "--control",
        "control",
    ]);
    assert!(ok, "{text}");
    let score: Value =
        serde_json::from_slice(&fs::read(Path::new(&out).join("score.json")).unwrap()).unwrap();
    let verdicts = &score["verdicts"];
    assert_eq!(verdicts["faster"]["pass"], true, "{text}");
    assert_eq!(verdicts["faster"]["games"].as_array().unwrap().len(), 2);
    assert_eq!(verdicts["slower"]["pass"], false, "{text}");
    assert!(
        !verdicts["slower"]["worse"].as_array().unwrap().is_empty(),
        "{text}"
    );
    assert_eq!(verdicts["same"]["pass"], false, "{text}");
    assert!(
        verdicts["same"]["worse"].as_array().unwrap().is_empty(),
        "{text}"
    );
    assert!(
        verdicts["same"]["better"].as_array().unwrap().is_empty(),
        "{text}"
    );
    assert_eq!(score["rows"].as_array().unwrap().len(), 12);
    let (ok, text) = bench(&[
        "calibrate",
        "--specs",
        specs,
        "--out",
        &out,
        "--control",
        "control",
    ]);
    assert!(ok && text.contains("| alpha-boss |"), "{text}");
}

#[test]
fn the_stub_adapter_is_deterministic_across_worker_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let spec = specs().join("beta-bridge.json");
    let (ok, text) = bench(&[
        "determinism",
        "--specs",
        spec.to_str().unwrap(),
        "--arms",
        arms.to_str().unwrap(),
        "--arm",
        "control",
        "--seed",
        "3",
        "--out",
        tmp.path().to_str().unwrap(),
    ]);
    assert!(ok && text.contains("determinism: PASS"), "{text}");
}

#[test]
fn a_spec_whose_tape_differs_from_its_hash_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut spec: Value =
        serde_json::from_slice(&fs::read(specs().join("alpha-door.json")).unwrap()).unwrap();
    fs::copy(
        specs().join("alpha-door.tape"),
        tmp.path().join("alpha-door.tape"),
    )
    .unwrap();
    spec["start"]["tape_sha256"] = json!("0".repeat(64));
    let path = tmp.path().join("alpha-door.json");
    fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let arms = arms(tmp.path());
    let (ok, text) = bench(&[
        "run",
        "--specs",
        path.to_str().unwrap(),
        "--arms",
        arms.to_str().unwrap(),
        "--arm",
        "control",
        "--seeds",
        "1",
        "--out",
        tmp.path().join("out").to_str().unwrap(),
    ]);
    assert!(!ok && text.contains("does not match tape_sha256"), "{text}");
}
