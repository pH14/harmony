// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

fn harmony() -> Command {
    Command::new(env!("CARGO_BIN_EXE_harmony"))
}

fn saved_fault_run(
    path: &std::path::Path,
    bundle: serde_json::Value,
    outcome: serde_json::Value,
    status: &str,
) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "format": "harmony-run-v4",
            "binary_identity": "saved-on-another-host",
            "architecture": "other-architecture",
            "mode": if bundle.is_null() { "run" } else { "search" },
            "status": status,
            "error": (status == "failed").then_some("guest application did not start"),
            "config": {
                "workload": {"package": "faults"},
                "runner": {"kind": "consonance", "backend": "uml"},
                "search": {"seed": 0, "executions": 1}
            },
            "parent": null,
            "artifacts": {},
            "payload": {"bundle": bundle, "actions": [], "settle": true, "outcome": outcome},
            "runner_identity": {}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn show_uses_saved_faults_outcome_when_command_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let recorded = serde_json::json!({"application_sha256": "saved"});
    saved_fault_run(
        dir.path(),
        serde_json::Value::Null,
        recorded.clone(),
        "complete",
    );
    std::fs::write(
        dir.path().join("run.json"),
        serde_json::to_vec(&serde_json::json!({"application_sha256": "altered"})).unwrap(),
    )
    .unwrap();

    let output = harmony()
        .args(["show", dir.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["result"], recorded);
}

#[test]
fn show_refuses_changed_command_console_with_saved_digest() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let original = b"saved console\n";
    let recorded = serde_json::json!({"serial_sha256": format!("{:x}", Sha256::digest(original))});
    saved_fault_run(dir.path(), serde_json::Value::Null, recorded, "complete");
    std::fs::write(dir.path().join("serial.log"), original).unwrap();
    let command = || {
        harmony()
            .args(["show", dir.path().to_str().unwrap(), "--logs", "--json"])
            .output()
            .unwrap()
    };
    let honest = command();
    assert!(
        honest.status.success(),
        "{}",
        String::from_utf8_lossy(&honest.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&honest.stdout).unwrap();
    assert_eq!(shown[0]["lines"], serde_json::json!(["saved console"]));
    std::fs::write(dir.path().join("serial.log"), b"altered console\n").unwrap();

    let output = command();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("recorded serial.log has changed"));
}

#[test]
fn show_refuses_changed_failed_command_console_with_saved_digest() {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let recorded =
        serde_json::json!({"serial_sha256": format!("{:x}", Sha256::digest(b"saved console\n"))});
    saved_fault_run(dir.path(), serde_json::Value::Null, recorded, "failed");
    std::fs::write(dir.path().join("serial.log"), b"altered console\n").unwrap();

    let output = harmony()
        .args(["show", dir.path().to_str().unwrap(), "--logs", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("recorded serial.log has changed"));
}

#[test]
fn show_timeline_and_logs_use_saved_faults_witness_when_report_changes() {
    use faults_workload::{
        package::{ReplayStep, ReplaySummary},
        target::FaultObservations,
    };

    let dir = tempfile::tempdir().unwrap();
    let observation = FaultObservations::default();
    let mut witness = ReplaySummary::from_observation(&observation, Some([7; 32]), 0, 0);
    witness.timeline.push(ReplayStep {
        step: 0,
        action: None,
        observation,
        console: "saved console".into(),
        settlement: false,
    });
    let recorded = serde_json::json!({
        "package": "faults", "mode": "search", "image_sha256": "image", "kernel_sha256": "kernel",
        "identity": "runner", "seed": 0, "workers": 1, "ram_mib": 1024, "executions": 1,
        "bug_found": false, "first_bug_execution": null, "bugs": [], "replays": [witness],
        "execution_ticks": 1, "wall_seconds": 1, "execution_failures": 0
    });
    saved_fault_run(
        dir.path(),
        serde_json::json!("node app"),
        recorded.clone(),
        "complete",
    );
    let mut altered = recorded.clone();
    altered["replays"][0]["timeline"][0]["console"] = "altered console".into();
    std::fs::write(
        dir.path().join("report.json"),
        serde_json::to_vec(&altered).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("serial.log"), b"planted console\n").unwrap();

    let path = dir.path().to_str().unwrap();
    for (args, expected) in [
        (vec!["show", path, "--json"], recorded.clone()),
        (
            vec!["show", path, "--timeline", "--json"],
            recorded["replays"][0]["timeline"].clone(),
        ),
    ] {
        let output = harmony().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let actual = if shown.is_array() {
            shown
        } else {
            shown["result"].clone()
        };
        assert_eq!(actual, expected);
    }
    let output = harmony()
        .args(["show", path, "--logs", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown[0]["step"], 0);
    assert_eq!(shown[0]["lines"], serde_json::json!(["saved console"]));
}

#[test]
fn initialize_then_inspect_configuration_without_guest_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let out = harmony()
        .current_dir(dir.path())
        .args(["init", "--language", "python"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(dir.path().join("harmony.toml")).unwrap();
    assert!(text.contains("python"));
    let second = harmony()
        .current_dir(dir.path())
        .args(["init"])
        .output()
        .unwrap();
    assert!(!second.status.success());
}

#[test]
fn malformed_inline_configuration_fails_before_runtime_provisioning() {
    let out = harmony()
        .args(["search", "--config-toml", "imag = 'bad'", "--offline"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown field"));
}

#[test]
fn unrepresentable_numeric_inputs_fail_before_creating_runs() {
    let dir = tempfile::tempdir().unwrap();
    for (flag, value) in [
        ("--seed", "9223372036854775808"),
        ("--executions", "9223372036854775808"),
        ("--for", "9223372036854775808s"),
    ] {
        let out = harmony()
            .current_dir(dir.path())
            .args([
                "search",
                "example:local",
                flag,
                value,
                "--name",
                "invalid",
                "--offline",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("maximum integer"));
        assert!(!dir.path().join(".harmony").exists());
    }
}
