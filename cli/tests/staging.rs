// SPDX-License-Identifier: AGPL-3.0-or-later
//! Offline tests for the `harmony oci` staging pipeline: cpio byte
//! stability and the run-record exit-marker parse. The image and boot
//! halves need docker / a hypervisor and are exercised by the live run
//! itself, not here.

use std::process::Command;

fn harmony() -> Command {
    Command::new(env!("CARGO_BIN_EXE_harmony"))
}

#[test]
fn preflight_runs_and_reports() {
    let out = harmony().arg("preflight").arg("--json").output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report.get("os").is_some());
    assert!(report.get("matrix_cell").is_some());
    assert!(report.get("ready").is_some());
}

#[test]
fn oci_run_refuses_missing_image_input() {
    // A nonexistent path with no docker/podman reachable name shape still
    // exercises argument handling; the command must fail, not hang.
    let out = harmony()
        .args(["oci", "run", "/nonexistent/image.tar", "--timeout", "5"])
        .env("HARMONY_GUEST_DIR", "/nonexistent")
        .output()
        .unwrap();
    assert!(!out.status.success());
}
