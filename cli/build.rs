// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CLI workspace root")
        .to_path_buf();
    let inputs = [
        "Cargo.toml",
        "Cargo.lock",
        "flake.nix",
        "flake.lock",
        "consonance/harmony-linux",
        "consonance/hypercall-proto",
        "consonance/hypercall-doorbell",
        "consonance/execution-proto",
        "consonance/process-proto",
        "cli/src/oci/mod.rs",
        "cli/src/oci/runner.rs",
        "workloads/faults/src/consonance.rs",
        "workloads/nes-machine/src/consonance.rs",
    ];
    for input in inputs {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    let script = root.join("consonance/harmony-linux/scripts/runtime-artifacts.py");
    let architecture = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let output = Command::new("python3")
        .arg(&script)
        .args(["source-key", "--repo"])
        .arg(&root)
        .args(["--architecture", &architecture])
        .output()
        .expect("run runtime-artifacts.py with python3");
    assert!(
        output.status.success(),
        "runtime source key failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let key = String::from_utf8(output.stdout).expect("UTF-8 runtime source key");
    let key = key.trim();
    assert!(
        key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid runtime source key"
    );
    println!("cargo:rustc-env=HARMONY_PLATFORM_SOURCE_DIGEST={key}");
}
