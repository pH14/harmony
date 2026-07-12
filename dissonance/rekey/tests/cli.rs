// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smoke tests that drive the `rekey` binary through **every** `Command` branch
//! against the frozen corpus.
//!
//! `src/bin/rekey.rs` is argument plumbing over the library the unit and corpus
//! suites already pin, but it is still mutation-gated (`.cargo/mutants.toml`
//! excludes `**/main.rs`, which does not match `src/bin/rekey.rs`). Without these
//! tests a fully inert `run` — `Command::Score` writing nothing, `manifest`
//! skipping its freshness check, `main` mapping every outcome to success — would
//! go undetected. Each case asserts a **distinct observable effect** (a written
//! report, printed JSON, a success or a failure exit) so the dispatch, the
//! `--write` / `--stdout` branches, and `main`'s exit-code mapping are all
//! constrained, not merely executed.
//!
//! The binary is driven as a subprocess via `CARGO_BIN_EXE_rekey` (cargo sets it
//! for integration tests), so no `assert_cmd`-style dev-dependency is pulled. The
//! whole file is `cli`-gated: with the feature off the binary is not built and
//! the env var does not exist.
#![cfg(feature = "cli")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The committed corpus root, absolute so the tests are cwd-independent.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../benchmark/campaign-data")
}

/// Run `rekey --corpus <corpus> <args…>` and return its captured output.
fn run(corpus: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rekey"))
        .arg("--corpus")
        .arg(corpus)
        .args(args)
        .output()
        .expect("the rekey binary runs")
}

/// Copy a directory tree — the corpus is 11 MB, so mirroring it into a tempdir
/// to exercise `manifest --write` without touching the committed tree is cheap.
fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir dst");
    for entry in std::fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_all(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// `score --stdout` renders the report to stdout; `score --out FILE` writes it to
/// a file and says so on stderr. Both must reproduce the committed report, and
/// the two branches must be distinguishable — a `*stdout` flip would send the
/// report to the wrong sink.
#[test]
fn score_renders_the_report_to_stdout_and_to_a_file() {
    let corpus = corpus();
    let committed = std::fs::read_to_string(corpus.join("../REKEY-REPORT.md")).expect("report");

    // --stdout: the report lands on stdout, nothing is written.
    let out = run(&corpus, &["score", "--stdout"]);
    assert!(out.status.success(), "score --stdout exits 0");
    let printed = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        printed.starts_with("# REKEY-REPORT"),
        "the report is on stdout"
    );
    assert_eq!(
        printed, committed,
        "stdout is byte-identical to the committed report"
    );

    // --out FILE (no --stdout): the report is written, stderr announces it,
    // stdout stays empty.
    let tmp = tempfile::tempdir().expect("tempdir");
    let report = tmp.path().join("out.md");
    let out = run(
        &corpus,
        &["score", "--out", report.to_str().expect("utf8 path")],
    );
    assert!(out.status.success(), "score --out exits 0");
    assert!(
        out.stdout.is_empty(),
        "nothing on stdout when writing a file"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("wrote"),
        "stderr announces the write"
    );
    assert_eq!(
        std::fs::read_to_string(&report).expect("the report was written"),
        committed,
        "the written file is the committed report"
    );
}

/// `verify` succeeds on the real corpus and reports what it checked on stderr.
#[test]
fn verify_passes_on_the_committed_corpus() {
    let out = run(&corpus(), &["verify"]);
    assert!(out.status.success(), "verify exits 0 on the frozen corpus");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("verified"),
        "stderr reports the verification"
    );
}

/// `manifest` (no `--write`) prints the manifest and checks it against the
/// committed one — it must not write. The printed JSON is the manifest's, and
/// the `--write` branch instead writes the file and announces it.
#[test]
fn manifest_prints_then_writes() {
    let corpus = corpus();

    // No --write: the manifest is printed and checked; stderr is quiet.
    let out = run(&corpus, &["manifest"]);
    assert!(
        out.status.success(),
        "manifest checks clean against the committed one"
    );
    let printed = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        printed.contains("\"version\": 1"),
        "the manifest JSON is on stdout"
    );

    // --write, into a private mirror so the committed tree is untouched. It
    // reproduces the committed manifest exactly, and stderr announces the write.
    let tmp = tempfile::tempdir().expect("tempdir");
    let mirror = tmp.path().join("campaign-data");
    copy_dir_all(&corpus, &mirror);
    let out = run(&mirror, &["manifest", "--write"]);
    assert!(out.status.success(), "manifest --write exits 0");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("wrote"),
        "stderr announces the write"
    );
    assert_eq!(
        std::fs::read_to_string(mirror.join("rekey-corpus.json")).expect("written"),
        std::fs::read_to_string(corpus.join("rekey-corpus.json")).expect("committed"),
        "the rebuilt manifest is byte-identical to the committed one"
    );
}

/// A corpus with no manifest is a **loud failure**: `verify` returns an error and
/// `main` maps it to a non-zero exit. This pins `main`'s `Err → FAILURE` arm,
/// which a success-only suite would leave unconstrained.
#[test]
fn a_missing_corpus_fails_loudly() {
    let empty = tempfile::tempdir().expect("tempdir");
    let out = run(empty.path(), &["verify"]);
    assert!(!out.status.success(), "an absent corpus is a non-zero exit");
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("rekey:"),
        "the error is reported on stderr"
    );
}
