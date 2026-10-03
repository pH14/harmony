// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

fn harmony() -> Command {
    Command::new(env!("CARGO_BIN_EXE_harmony"))
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
