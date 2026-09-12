// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

const PINNED_NIGHTLY: &str = "nightly-2026-06-16";
const CRATE: &str = "hypercall-doorbell";

#[test]
#[ignore = "needs pinned nightly + cargo-public-api; runs in the public-api CI job via `cargo test -- --ignored`"]
fn public_api_matches_snapshot() {
    let toolchain = format!("+{PINNED_NIGHTLY}");
    let output = match Command::new("cargo")
        .args([
            &toolchain,
            "public-api",
            "-p",
            CRATE,
            "--all-features",
            "-sss",
            "--color",
            "never",
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("SKIP: {CRATE} public-api test — cannot exec cargo ({e})");
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let absent = stderr.contains("no such command")
            || stderr.contains("is not installed")
            || stderr.contains("toolchain may not be installed")
            || stderr.contains("does not exist")
            || stderr.contains("failed to install")
            || stderr.contains("could not rename")
            || stderr.contains("component download failed")
            || stderr.contains("detected conflict");
        if absent {
            eprintln!("SKIP: {CRATE} public-api test — tooling absent:\n{stderr}");
            return;
        }
        panic!("cargo public-api failed for {CRATE}:\n{stderr}");
    }

    let actual = String::from_utf8(output.stdout).expect("public-api output is UTF-8");
    let snapshot_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/public-api.txt");

    if std::env::var_os("UPDATE_PUBLIC_API").is_some() {
        std::fs::write(snapshot_path, &actual).expect("write snapshot");
        eprintln!("updated {snapshot_path}");
        return;
    }

    let expected = std::fs::read_to_string(snapshot_path).expect("read snapshot");
    assert_eq!(
        expected.trim_end(),
        actual.trim_end(),
        "public API of `{CRATE}` drifted from tests/public-api.txt. If this          change is intentional and reviewed, refresh the snapshot with:\n           UPDATE_PUBLIC_API=1 cargo test -p {CRATE} --test public_api"
    );
}
