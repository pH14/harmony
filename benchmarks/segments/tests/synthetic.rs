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

const SEEDS: &str = "1-32";

#[test]
fn the_verdict_separates_faster_slower_and_same_arms_at_32_seeds() {
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
        "slower", "--arm", "same", "--seeds", SEEDS, "--jobs", "8", "--out", &out,
    ];
    let (ok, text) = bench(&run);
    assert!(ok, "{text}");
    assert!(
        text.contains("384 runs: 0 already have a result, 384 to run"),
        "{text}"
    );
    let (ok, text) = bench(&run);
    assert!(
        ok && text.contains("384 runs: 384 already have a result, 0 to run"),
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

fn spec_with_budget(dir: &Path, budget: u64) -> PathBuf {
    let mut spec: Value =
        serde_json::from_slice(&fs::read(specs().join("alpha-door.json")).unwrap()).unwrap();
    spec["work_budget"] = json!(budget);
    let specs_dir = dir.join("specs");
    fs::create_dir_all(&specs_dir).unwrap();
    fs::copy(
        specs().join("alpha-door.tape"),
        specs_dir.join("alpha-door.tape"),
    )
    .unwrap();
    let path = specs_dir.join("alpha-door.json");
    fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
    path
}

fn stops(out: &Path) -> Vec<(String, u64)> {
    (1..=16)
        .map(|seed| {
            let result: Value = serde_json::from_slice(
                &fs::read(out.join(format!("alpha-door/control/s{seed}/result.json"))).unwrap(),
            )
            .unwrap();
            (
                result["stop"].as_str().unwrap().to_owned(),
                result["work"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_budget_change_reruns_only_the_seeds_it_affects() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let out = tmp.path().join("out");
    let run = |budget: u64| {
        bench(&[
            "run",
            "--specs",
            spec_with_budget(tmp.path(), budget).to_str().unwrap(),
            "--arms",
            arms.to_str().unwrap(),
            "--arm",
            "control",
            "--seeds",
            "1-16",
            "--out",
            out.to_str().unwrap(),
        ])
    };
    let small = 1 << 21;
    let large = 1 << 24;
    let (ok, text) = run(small);
    assert!(ok, "{text}");
    let first = stops(&out);
    let reached = first.iter().filter(|(stop, _)| stop == "milestone").count();
    assert!(reached > 0 && reached < 16, "{first:?}");
    let (ok, text) = run(large);
    assert!(
        ok && text.contains(&format!(
            "16 runs: {reached} already have a result, {} to run",
            16 - reached
        )),
        "{text}"
    );
    let second = stops(&out);
    for (before, after) in first.iter().zip(&second) {
        if before.0 == "milestone" {
            assert_eq!(before, after);
        }
    }
    let at_most_small = second
        .iter()
        .filter(|(stop, work)| stop == "budget" || *work <= small)
        .count();
    let (ok, text) = run(small);
    assert!(
        ok && text.contains(&format!(
            "16 runs: {at_most_small} already have a result, {} to run",
            16 - at_most_small
        )),
        "{text}"
    );
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
