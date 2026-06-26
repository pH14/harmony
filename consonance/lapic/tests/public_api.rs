// SPDX-License-Identifier: AGPL-3.0-or-later
//! Frozen public-API snapshot guard — see `docs/CODE-QUALITY.md`
//! ("Public-API snapshots") and `tasks/00-CONVENTIONS.md` rule 3.
//!
//! Regenerates this crate's public surface with `cargo public-api` on the
//! pinned nightly toolchain and asserts it byte-matches the committed
//! `tests/public-api.txt`. Any drift in the frozen public contract becomes a
//! failing test and a reviewable diff. `lapic`'s public surface (the
//! `LapicState` snapshot seam task 09 consumes, plus the register-offset
//! constants `vmm-core` addresses the device by) is a contract.
//!
//! Refresh after an intentional, reviewed API change:
//!   `UPDATE_PUBLIC_API=1 cargo test -p lapic --test public_api`
//!
//! Requires the pinned nightly toolchain and `cargo-public-api`
//! (`scripts/install-quality-tools.sh`). When either is absent the test skips
//! loudly rather than failing, so a plain `cargo nextest` on a stable-only box
//! stays green; CI installs both, so the gate runs for real there.

use std::process::Command;

/// Pinned nightly — `cargo-public-api` needs rustdoc-JSON, which is
/// nightly-only. Keep in sync with `docs/CODE-QUALITY.md` and the `public-api`
/// CI job.
const PINNED_NIGHTLY: &str = "nightly-2026-06-16";
const CRATE: &str = "lapic";

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
            // Freeze the full surface (all features), so a public item gated
            // behind a non-default feature can never drift unnoticed.
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
        // Missing tool / toolchain -> skip; a real build error -> fail.
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
        "public API of `{CRATE}` drifted from tests/public-api.txt. If this change is intentional and reviewed, refresh the snapshot with:\n           UPDATE_PUBLIC_API=1 cargo test -p {CRATE} --test public_api"
    );
}
