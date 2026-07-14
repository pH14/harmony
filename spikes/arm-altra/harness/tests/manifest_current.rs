//! Generator test for the committed expected-count manifest.
//!
//! Deliverable 1 requires the expected-count manifest to be *generator-tested*:
//! the committed `payloads/expected/expected-counts.json` must be exactly what
//! `arm-scan manifest` emits today. If someone changes a payload's trip count, its
//! branch sequence, or the oracle's derivation without regenerating the manifest,
//! this fails — so the manifest can never silently drift away from the model it
//! claims to describe.
//!
//! Regenerate with:
//!   cargo run -p arm-harness --bin arm-scan -- manifest \
//!     > ../payloads/expected/expected-counts.json

use std::process::Command;

#[test]
fn committed_manifest_is_current() {
    let exe = env!("CARGO_BIN_EXE_arm-scan");
    let out = Command::new(exe)
        .arg("manifest")
        .output()
        .expect("run arm-scan manifest");
    assert!(
        out.status.success(),
        "arm-scan manifest exited nonzero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let generated = String::from_utf8(out.stdout).expect("manifest is utf-8");

    // The committed manifest lives beside the payloads, one tree over from the
    // harness crate.
    let committed_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../payloads/expected/expected-counts.json"
    );
    let committed = std::fs::read_to_string(committed_path)
        .expect("committed manifest exists (regenerate it if this is a fresh checkout)");

    assert_eq!(
        generated.trim_end(),
        committed.trim_end(),
        "expected-counts.json is stale — regenerate with `arm-scan manifest`"
    );
}
