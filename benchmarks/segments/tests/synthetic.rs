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
    write_spec(dir, |spec| spec["work_budget"] = json!(budget))
}

fn write_spec(dir: &Path, change: impl Fn(&mut Value)) -> PathBuf {
    let mut spec: Value =
        serde_json::from_slice(&fs::read(specs().join("alpha-door.json")).unwrap()).unwrap();
    change(&mut spec);
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

fn run_arms(spec: &Path, arms: &Path, names: &[&str], seeds: &str, out: &Path) -> String {
    let mut args = vec![
        "run",
        "--specs",
        spec.to_str().unwrap(),
        "--arms",
        arms.to_str().unwrap(),
        "--seeds",
        seeds,
        "--out",
        out.to_str().unwrap(),
    ];
    for name in names {
        args.extend(["--arm", name]);
    }
    let (ok, text) = bench(&args);
    assert!(ok, "{text}");
    text
}

fn score_and_calibrate(spec: &Path, out: &Path) -> (Value, String, String) {
    let common = [
        "--specs",
        spec.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--control",
        "control",
    ];
    let (ok, scored) = bench(&[&["score"][..], &common].concat());
    assert!(ok, "{scored}");
    let (ok, calibrated) = bench(&[&["calibrate"][..], &common].concat());
    assert!(ok, "{calibrated}");
    let score = serde_json::from_slice(&fs::read(out.join("score.json")).unwrap()).unwrap();
    (score, scored, calibrated)
}

#[test]
fn results_from_an_earlier_milestone_are_left_out_as_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let out = tmp.path().join("out");
    let spec = write_spec(tmp.path(), |_| {});
    run_arms(&spec, &arms, &["control", "faster"], "1-8", &out);
    let spec = write_spec(tmp.path(), |spec| spec["milestone"][0]["eq"] = json!(6));
    run_arms(&spec, &arms, &["faster"], "1-8", &out);
    let (score, text, _) = score_and_calibrate(&spec, &out);
    assert_eq!(score["stale"]["alpha-door"]["control"], 8, "{text}");
    assert!(
        score["stale"]["alpha-door"].get("faster").is_none(),
        "{text}"
    );
    let rows = score["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{text}");
    assert_eq!(rows[0]["arm"], "faster");
    assert!(
        text.contains("Stale results, left out: alpha-door control: 8"),
        "{text}"
    );
}

#[test]
fn memory_stops_are_counted_and_flag_the_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let out = tmp.path().join("out");
    let spec = write_spec(tmp.path(), |_| {});
    run_arms(&spec, &arms, &["control"], "1-4", &out);
    fs::write(
        out.join("alpha-door/control/s2/result.json"),
        json!({"stop": "memory", "work": 1000, "job_stream_sha256": "", "message": ""}).to_string(),
    )
    .unwrap();
    let (score, text, calibrated) = score_and_calibrate(&spec, &out);
    assert_eq!(score["rows"][0]["memory_stops"], 1, "{text}");
    assert!(text.contains("Memory stops on alpha-door: 1."), "{text}");
    assert!(calibrated.contains("raise memory_limit"), "{calibrated}");
}

#[test]
fn duplicate_seeds_run_once() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let spec = write_spec(tmp.path(), |_| {});
    let text = run_arms(&spec, &arms, &["control"], "1,1,2", &tmp.path().join("out"));
    assert!(
        text.contains("2 runs: 0 already have a result, 2 to run"),
        "{text}"
    );
}

#[test]
fn determinism_runs_the_adapter_again_instead_of_reading_old_results() {
    let tmp = tempfile::tempdir().unwrap();
    let arms = arms(tmp.path());
    let spec = specs().join("beta-bridge.json");
    let args = [
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
    ];
    let (ok, text) = bench(&args);
    assert!(ok && text.contains("determinism: PASS"), "{text}");
    let result = tmp
        .path()
        .join("determinism/beta-bridge/control/s3-w4/result.json");
    fs::write(
        &result,
        json!({"stop": "milestone", "work": 1, "job_stream_sha256": "x", "message": ""})
            .to_string(),
    )
    .unwrap();
    let (ok, text) = bench(&args);
    assert!(ok && text.contains("determinism: PASS"), "{text}");
    let rerun: Value = serde_json::from_slice(&fs::read(&result).unwrap()).unwrap();
    assert_ne!(rerun["work"], 1);
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
