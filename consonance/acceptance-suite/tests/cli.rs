// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — the `acceptance-suite` binary: real `std::process::Command` runs over toy
//! manifests. Parses the JSON report, asserts the exit code and the per-oracle
//! results, that one failing item flips the exit to 2, and (the anti-vacuity
//! cases) that empty/typo'd runs fail loudly and O3 under the default seed
//! actually distinguishes an RNG payload from a pure one.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use unison::{Subject, SubjectFactory};

const BIN: &str = env!("CARGO_BIN_EXE_acceptance-suite");

/// Write `contents` to `dir/name` and return its absolute path as a String.
fn write(dir: &Path, name: &str, contents: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path.to_str().unwrap().to_string()
}

fn run(args: &[&str]) -> (i32, Value) {
    let out = Command::new(BIN)
        .args(args)
        .output()
        .expect("spawn acceptance-suite");
    let code = out.status.code().expect("exit code");
    let stdout = String::from_utf8(out.stdout).unwrap();
    // `validate` and the error paths print non-JSON; tolerate that.
    let json = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (code, json)
}

/// Find item `name`'s result for oracle token `oracle`, return its `passed`.
fn passed(report: &Value, name: &str, oracle: &str) -> bool {
    let item = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["name"] == name)
        .unwrap_or_else(|| panic!("item {name} not in report"));
    item["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["oracle"] == oracle)
        .unwrap_or_else(|| panic!("oracle {oracle} not run for {name}"))["passed"]
        .as_bool()
        .unwrap()
}

/// First toy `source` (as a string) whose generated program is honest-RNG
/// (control-flow-stable: equal work across seeds, but observable output varies).
/// Found via the shared `acceptance_suite::toy_factory`, so it cannot drift from what
/// the binary builds; panics loudly if none exists in range.
fn rng_source() -> String {
    find_source(|out_differs, work_eq| out_differs && work_eq, "honest-RNG")
}

/// First toy `source` whose generated program is seed-pure (output identical
/// across seeds, equal work).
fn pure_source() -> String {
    find_source(|out_differs, work_eq| !out_differs && work_eq, "seed-pure")
}

fn find_source(pred: impl Fn(bool, bool) -> bool, what: &str) -> String {
    for n in 0u64..2000 {
        let s = n.to_string();
        let f = acceptance_suite::toy_factory(&s);
        let (mut m1, mut m2) = (f.spawn(1), f.spawn(2));
        m1.run_to(1_000_000).unwrap();
        m2.run_to(1_000_000).unwrap();
        let out_differs = m1.observable_digest() != m2.observable_digest();
        let work_eq = m1.work() == m2.work();
        if pred(out_differs, work_eq) {
            return s;
        }
    }
    panic!("no {what} toy source found in range");
}

#[test]
fn all_pass_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = write(
        dir.path(),
        "ok.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]

[[item]]
name = \"beta\"
kind = \"micro\"
source = \"2\"
oracles = [\"determinism\"]
",
    );

    let (code, report) = run(&["run", "--manifest", &manifest]);
    assert_eq!(code, 0, "report: {report}");
    assert_eq!(report["all_passed"], Value::Bool(true));
    assert!(passed(&report, "alpha", "determinism"));
    assert!(passed(&report, "beta", "determinism"));
}

#[test]
fn one_failing_item_flips_exit_to_two() {
    let dir = tempfile::tempdir().unwrap();
    // A 64-char hex digest that the real run will never produce.
    let wrong_golden = write(dir.path(), "wrong.hex", &"0".repeat(64));
    let manifest = write(
        dir.path(),
        "fail.toml",
        &format!(
            "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]

[[item]]
name = \"beta\"
kind = \"micro\"
source = \"2\"
oracles = [\"determinism\", \"conformance\"]
golden = \"{wrong_golden}\"
"
        ),
    );

    let (code, report) = run(&["run", "--manifest", &manifest]);
    assert_eq!(code, 2, "one failing oracle must flip exit to 2: {report}");
    assert_eq!(report["all_passed"], Value::Bool(false));
    // Determinism still passes for both; only beta's conformance fails.
    assert!(passed(&report, "alpha", "determinism"));
    assert!(passed(&report, "beta", "determinism"));
    assert!(!passed(&report, "beta", "conformance"));
}

#[test]
fn item_filter_runs_one_item() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = write(
        dir.path(),
        "two.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]

[[item]]
name = \"beta\"
kind = \"micro\"
source = \"2\"
oracles = [\"determinism\"]
",
    );
    let (code, report) = run(&["run", "--manifest", &manifest, "--item", "beta"]);
    assert_eq!(code, 0);
    let items = report["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "beta");
}

/// Finding 2: `--item <typo>` must NOT exit 0 all_passed (vacuous on an empty
/// `.all()`); it fails loudly because nothing ran.
#[test]
fn item_filter_typo_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = write(
        dir.path(),
        "two.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]
",
    );
    let (code, report) = run(&["run", "--manifest", &manifest, "--item", "nope"]);
    assert_eq!(code, 1, "a filter that matches nothing must fail loudly");
    assert_eq!(
        report,
        Value::Null,
        "no all_passed JSON on the nothing-ran path"
    );
}

/// Finding 1: an empty corpus (here via a typo'd `[[items]]` key, which
/// `deny_unknown_fields` rejects at parse time) must not run as a vacuous pass.
#[test]
fn empty_and_typoed_manifests_fail_loudly() {
    let dir = tempfile::tempdir().unwrap();

    // Genuinely empty manifest: run errors.
    let empty = write(dir.path(), "empty.toml", "");
    let (code, _) = run(&["run", "--manifest", &empty]);
    assert_eq!(code, 1, "empty manifest run must fail loudly");

    // Typo'd array key `[[items]]`: would silently parse to an empty corpus
    // without deny_unknown_fields. Now a hard parse error.
    let typo = write(
        dir.path(),
        "typo.toml",
        "\
[[items]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]
",
    );
    let (code, _) = run(&["run", "--manifest", &typo]);
    assert_eq!(
        code, 1,
        "typo'd manifest must be a hard error, not empty pass"
    );
    let (code, _) = run(&["validate", "--manifest", &typo]);
    assert_eq!(code, 1, "validate also errors on the typo'd manifest");
}

/// An item declaring `oracles = []` runs nothing; it must be rejected, not
/// aggregated as a vacuous all_passed (run errors loudly, validate rejects).
#[test]
fn item_with_no_oracles_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = write(
        dir.path(),
        "inert.toml",
        "\
[[item]]
name = \"inert\"
kind = \"micro\"
source = \"1\"
oracles = []
",
    );
    let (code, _) = run(&["run", "--manifest", &manifest]);
    assert_eq!(code, 1, "an item with no oracles must be rejected by run");
    let (code, _) = run(&["validate", "--manifest", &manifest]);
    assert_eq!(code, 2, "validate must reject an item with no oracles");
}

/// `--limit 0` verifies nothing, so it could only be vacuously green: rejected.
#[test]
fn limit_zero_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = write(
        dir.path(),
        "ok.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]
",
    );
    let (code, report) = run(&["run", "--manifest", &manifest, "--limit", "0"]);
    assert_eq!(
        code, 1,
        "--limit 0 must be rejected, not run as a vacuous pass"
    );
    assert_eq!(report, Value::Null);
}

/// Finding 3: under the DEFAULT seed (0), O3 must actually distinguish an honest
/// RNG payload from a pure one — i.e. the derived `seed_b` is effectively
/// distinct from `seed_a` after the toy registry normalizes 0 to ZERO_SEED_STATE.
/// Under the old `seed_b` collision, the RNG item would have falsely FAILED.
#[test]
fn o3_under_default_seed_distinguishes_rng_from_pure() {
    let dir = tempfile::tempdir().unwrap();
    let rng = rng_source();
    let pure = pure_source();
    let manifest = write(
        dir.path(),
        "o3.toml",
        &format!(
            "\
[[item]]
name = \"rng\"
kind = \"micro\"
source = \"{rng}\"
oracles = [\"seed_sensitivity:rng\"]

[[item]]
name = \"pure\"
kind = \"micro\"
source = \"{pure}\"
oracles = [\"seed_sensitivity:pure\"]
"
        ),
    );
    // No --seed: exercises the default seed = 0 path.
    let (code, report) = run(&["run", "--manifest", &manifest]);
    assert_eq!(
        code, 0,
        "honest RNG + pure must both pass under defaults: {report}"
    );
    assert!(
        passed(&report, "rng", "seed_sensitivity:rng"),
        "honest RNG payload must pass O3 under the default seed (no effective-seed collision)"
    );
    assert!(passed(&report, "pure", "seed_sensitivity:pure"));
}

/// A user-supplied --seed-b that collides with --seed after toy normalization is
/// rejected, not run as a vacuous comparison.
#[test]
fn colliding_user_seeds_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = write(
        dir.path(),
        "x.toml",
        "\
[[item]]
name = \"a\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]
",
    );
    // seed 0 normalizes to ZERO_SEED_STATE == 11400714819323198485.
    let (code, _) = run(&[
        "run",
        "--manifest",
        &manifest,
        "--seed",
        "0",
        "--seed-b",
        "11400714819323198485",
    ]);
    assert_eq!(code, 1, "effective-seed collision must be rejected");
}

#[test]
fn validate_accepts_good_and_rejects_missing_golden_and_empty() {
    let dir = tempfile::tempdir().unwrap();

    let good = write(
        dir.path(),
        "good.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\", \"conformance\"]
golden = \"consonance/acceptance-suite/golden/alpha.digest\"
",
    );
    let (code, _) = run(&["validate", "--manifest", &good]);
    assert_eq!(code, 0);

    let bad = write(
        dir.path(),
        "bad.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"conformance\"]
",
    );
    let (code, _) = run(&["validate", "--manifest", &bad]);
    assert_eq!(code, 2);

    // Empty corpus: validate must reject it.
    let empty = write(dir.path(), "empty.toml", "");
    let (code, _) = run(&["validate", "--manifest", &empty]);
    assert_eq!(code, 2, "validate must reject an empty corpus");
}

// ---------------------------------------------------------------------------
// The acceptance matrix as data: host selection, and the portable smoke test.
// ---------------------------------------------------------------------------

/// The real acceptance matrix, relative to this crate's directory (cargo runs a
/// test with the crate root as its working directory).
const REAL_MANIFEST: &str = "../../docs/corpus-manifest.toml";

/// **The CI smoke test for the packaged entry point.** The `acceptance-suite`
/// binary must run *every* portable cell of the real matrix on any machine —
/// otherwise the entry point rots green while only the hardware lane exercises
/// it, which is exactly the failure the ladder exists to prevent.
///
/// What is asserted, and why it is not stronger: under `--host portable` every
/// cell runs on the **toy** registry, so the O2 conformance goldens (real
/// digests captured from the patched backend on the box) cannot match. That is
/// expected and shows up as an oracle failure, not an operational one. So the
/// test requires:
///
/// * exit `0` or `2` — never `1`, the operational-error code. A `1` means the
///   entry point could not even run the matrix;
/// * every manifest cell present in the report and none listed unrun;
/// * every **O1 determinism** result green — the oracle the toy registry can
///   genuinely answer.
#[test]
fn the_binary_runs_every_portable_cell_of_the_real_matrix() {
    let (code, report) = run(&["run", "--manifest", REAL_MANIFEST, "--host", "portable"]);
    assert!(
        code == 0 || code == 2,
        "exit {code}: the portable matrix must run to a verdict, not an operational error"
    );
    assert_eq!(report["host"], "portable");
    assert_eq!(
        report["unrun"].as_array().unwrap().len(),
        0,
        "every cell of the real matrix lists `portable`; an unrun cell means the host axis \
         and the manifest have drifted apart"
    );

    let manifest = std::fs::read_to_string(REAL_MANIFEST).unwrap();
    let expected = acceptance_suite::load_manifest(&manifest).unwrap();
    let items = report["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        expected.len(),
        "every manifest cell must appear in the report"
    );
    for cell in &expected {
        let got = items
            .iter()
            .find(|it| it["name"] == cell.name)
            .unwrap_or_else(|| panic!("cell {} missing from the report", cell.name));
        assert!(
            !got["results"].as_array().unwrap().is_empty(),
            "cell {} ran no oracle",
            cell.name
        );
        assert!(
            passed(&report, &cell.name, "determinism"),
            "cell {} must pass O1 determinism on the toy registry",
            cell.name
        );
    }
}

#[test]
fn a_hardware_host_is_refused_loudly_rather_than_run_on_the_toy_registry() {
    // The worst available outcome would be a toy run reported under a hardware
    // host's name. On a build with no real-VMM registry the request must be an
    // operational error (exit 1); on one that has it, the missing payloads /
    // absent patched KVM make it exit 1 too. Either way: never a false green.
    let (code, _) = run(&["run", "--manifest", REAL_MANIFEST, "--host", "det-cfl-v1"]);
    assert_eq!(
        code, 1,
        "a hardware host on a machine that cannot serve it is an operational error"
    );
}

#[test]
fn an_unknown_host_is_an_error_not_an_empty_run() {
    let (code, _) = run(&["run", "--manifest", REAL_MANIFEST, "--host", "nope"]);
    assert_eq!(code, 1, "an unknown --host must be rejected");
}

#[test]
fn a_cell_no_selected_host_satisfies_is_reported_unrun_never_passed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "mixed.toml",
        "\
[[item]]
name = \"here\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]
hosts = [\"portable\"]
virt = \"l1\"

[[item]]
name = \"elsewhere\"
kind = \"micro\"
source = \"2\"
oracles = [\"determinism\"]
hosts = [\"det-cfl-v1\"]
virt = \"l1\"
",
    );
    let (code, report) = run(&["run", "--manifest", &path, "--host", "portable"]);
    assert_eq!(code, 0);
    assert_eq!(report["items"].as_array().unwrap().len(), 1);
    assert_eq!(report["items"][0]["name"], "here");
    assert_eq!(
        report["unrun"].as_array().unwrap(),
        &vec![serde_json::json!("elsewhere")],
        "the cell this host cannot run must be named, not silently dropped"
    );
}

#[test]
fn an_unknown_host_token_in_the_manifest_is_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "typo.toml",
        "\
[[item]]
name = \"alpha\"
kind = \"micro\"
source = \"1\"
oracles = [\"determinism\"]
hosts = [\"det-cfl-v2\"]
",
    );
    // A typo'd host must be loud: silently, it would be a cell that no run ever
    // executes and no report ever mentions.
    let (code, _) = run(&["run", "--manifest", &path, "--host", "portable"]);
    assert_eq!(code, 1, "an unknown host token must fail the parse");
    let (code, _) = run(&["validate", "--manifest", &path]);
    assert_eq!(code, 1, "validate must reject it too");
}
