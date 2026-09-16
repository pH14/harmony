#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Focused tests for platform boundaries and workflow timeouts."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from unittest import mock
import json
import copy
import contextlib
import io
from pathlib import Path


SCRIPT = Path(__file__).with_name("custom-lints.py")
SPEC = importlib.util.spec_from_file_location("custom_lints", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
LINTS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LINTS
SPEC.loader.exec_module(LINTS)


def workflow(job: str, body: str) -> str:
    return f"""name: Checks
on:
  pull_request:
jobs:
  {job}:
    name: Bounded validation
    runs-on: ubuntu-latest
{body}
"""


class PlatformBoundaryLintTests(unittest.TestCase):
    def test_new_platform_components_are_in_scope(self) -> None:
        rules = {rule.name: rule for rule in LINTS.RULES}
        for path in ("consonance/execution-proto/src/lib.rs", "consonance/future-component/README.md"):
            self.assertTrue(rules["consonance-no-workload-names"].applies(path))
        for path in ("consonance/harmony-linux/supervisor/src/main.rs",
                     "consonance/harmony-linux/runtime/init.sh",
                     "consonance/harmony-linux/README.md"):
            self.assertTrue(rules["guest-linux-no-workload-names"].applies(path))
            self.assertFalse(rules["consonance-no-workload-names"].applies(path))
        self.assertFalse(rules["guest-linux-no-workload-names"].applies("workloads/example/init.sh"))
        self.assertFalse(rules["consonance-no-workload-names"].applies("consonance-extra/example.rs"))

    def test_fault_package_names_are_workload_specific(self) -> None:
        self.assertIsNotNone(LINTS.WORKLOAD_NAME_RE.search("nes"))
        self.assertIsNotNone(LINTS.WORKLOAD_NAME_RE.search("faultlab"))
        self.assertIsNotNone(LINTS.WORKLOAD_NAME_RE.search("fault-library"))
        for path in (
            "consonance/harmony-linux/faultlab-init.sh",
            "consonance/harmony-linux/fault-library-config",
        ):
            self.assertIsNotNone(LINTS.MISPLACED_WORKLOAD_FILE_RE.search(path))


class WorkflowTimeoutLintTests(unittest.TestCase):
    def check(self, content: str) -> list[LINTS.Violation]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workflow_path = root / ".github/workflows/quality.yml"
            workflow_path.parent.mkdir(parents=True)
            workflow_path.write_text(content)
            with mock.patch.object(LINTS, "check_pr_check_routing", return_value=[]):
                return LINTS.check_workflow_rules(root, [".github/workflows/quality.yml"])

    def test_missing_timeout_is_a_violation(self) -> None:
        violations = self.check(workflow("missing", "    steps: []"))
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout"])
        self.assertIn("no timeout-minutes", violations[0].text)

    def test_timeout_at_or_below_fifteen_minutes_passes(self) -> None:
        for minutes in (1, 15):
            with self.subTest(minutes=minutes):
                self.assertFalse(
                    self.check(workflow("bounded", f"    timeout-minutes: {minutes}\n    steps: []"))
                )

    def test_timeout_above_fifteen_minutes_is_a_violation(self) -> None:
        violations = self.check(workflow("too_long", "    timeout-minutes: 16\n    steps: []"))
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout"])
        self.assertIn("timeout-minutes=16", violations[0].text)

    def test_non_pr_guard_requires_a_separate_workflow(self) -> None:
        content = workflow(
            "deep",
            "    if: github.event_name == 'schedule'\n    timeout-minutes: 90\n    steps: []",
        )
        self.assertEqual([v.rule for v in self.check(content)], ["ci-pr-only-jobs"])

    def test_negative_event_comparison_does_not_prove_non_pr(self) -> None:
        content = workflow(
            "ambiguous",
            "    if: github.event_name != 'schedule'\n    steps: []",
        )
        violations = self.check(content)
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout"])

    def test_negated_positive_equality_does_not_prove_non_pr(self) -> None:
        content = workflow(
            "negated",
            "    if: ${{ !(github.event_name == 'schedule') }}\n    steps: []",
        )
        violations = self.check(content)
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout"])

    def test_nested_or_with_pr_branch_does_not_prove_non_pr(self) -> None:
        content = workflow(
            "mixed",
            "    if: always() && (github.event_name == 'schedule' || github.event_name == 'pull_request')\n"
            "    steps: []",
        )
        violations = self.check(content)
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout"])

    def test_nested_non_pr_branches_prove_non_pr(self) -> None:
        content = workflow(
            "deep",
            "    if: always() && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')\n"
            "    timeout-minutes: 90\n"
            "    steps: []",
        )
        self.assertEqual([v.rule for v in self.check(content)], ["ci-pr-only-jobs"])

    def test_adjacent_named_exception_cannot_bypass_timeout(self) -> None:
        content = workflow(
            "mutants",
            """    # ci-pr-job-timeout-exception: mutants -- sharded mutation coverage is required
    timeout-minutes: 90
    if: github.event_name == 'pull_request'
    steps: []""",
        )
        self.assertEqual([v.rule for v in self.check(content)], ["ci-pr-job-timeout"])

    def test_exception_elsewhere_in_job_does_not_apply(self) -> None:
        content = workflow(
            "mutants",
            """    # ci-pr-job-timeout-exception: mutants -- this is deliberately misplaced
    steps: []
    timeout-minutes: 90""",
        )
        violations = self.check(content)
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout"])

    def test_exception_for_another_job_does_not_apply(self) -> None:
        content = f"""name: Checks
on:
  pull_request:
jobs:
  other:
    name: Other validation
    runs-on: ubuntu-latest
    # ci-pr-job-timeout-exception: mutants -- wrong job name
    timeout-minutes: 90
    steps: []
  mutants:
    name: Mutation testing
    runs-on: ubuntu-latest
    timeout-minutes: 90
    steps: []
"""
        violations = self.check(content)
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout", "ci-pr-job-timeout"])

    def test_parse_errors_fail_closed(self):
        for content in ("name: Checks / broken\njobs: [", "[]", "name: Checks / absent\non: pull_request",
                        "name: Checks / bad\non: pull_request\njobs: {broken: null}",
                        "name: Checks / bad\non: pull_request\njobs: {broken: {steps: null}}"):
            with self.subTest(content=content):
                self.assertIn("ci-workflow-parse", [v.rule for v in self.check(content)])

    def test_duplicate_keys_are_rejected(self):
        content = workflow("test", "    timeout-minutes: 90\n    timeout-minutes: 15")
        self.assertIn("ci-workflow-parse", [v.rule for v in self.check(content)])

    def test_missing_parser_is_an_error(self):
        with mock.patch.dict(sys.modules, {"yaml": None}):
            self.assertIn("ci-workflow-parse", [v.rule for v in self.check(workflow("job", "    timeout-minutes: 15"))])

    def test_category_controls_triggers_even_with_short_jobs(self):
        for category in ("Benchmarks", "Acceptance", "Nightly"):
            content = workflow("short", "    timeout-minutes: 1").replace("name: Checks", "name: " + category + " / Search campaign")
            self.assertIn("ci-workflow-triggers", [v.rule for v in self.check(content)])

    def test_checks_cannot_mix_schedule_and_pr(self):
        content = workflow("short", "    timeout-minutes: 1").replace("  pull_request:", "  pull_request:\n  schedule: [{cron: '0 6 * * *'}]")
        self.assertIn("ci-workflow-triggers", [v.rule for v in self.check(content)])

    def test_deep_tools_cannot_hide_behind_short_timeout(self):
        for command in ("cargo mutants --in-diff pr.diff", "cargo llvm-cov nextest", "cargo +nightly mutants",
                        "bash scripts/coverage.sh", "./scripts/coverage.sh"):
            content = workflow("short", f"    timeout-minutes: 1\n    steps:\n      - run: {command}")
            self.assertIn("ci-pr-extended-validation", [v.rule for v in self.check(content)])

    def test_pr_event_forms_and_nonpositive_timeouts(self):
        for event in ("pull_request", "pull_request_target", "merge_group"):
            for minutes in (0, -1, 16):
                content = workflow("bad", f"    timeout-minutes: {minutes}").replace("  pull_request:", f"  {event}:")
                self.assertIn("ci-pr-job-timeout", [v.rule for v in self.check(content)])

    def test_ci_errors_cannot_be_hidden_in_baseline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = ".github/workflows/test.yml"
            (root / path).parent.mkdir(parents=True)
            (root / path).write_text(workflow("long", "    timeout-minutes: 90"))
            with mock.patch.object(LINTS, "tracked_files", return_value=[path]), \
                 mock.patch.object(LINTS, "load_baseline", return_value={"ci-pr-job-timeout": [path + ":0"]}), \
                 mock.patch.object(LINTS, "save_baseline") as save, \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(LINTS.main(["--repo-root", directory]), 1)
                self.assertEqual(LINTS.main(["--repo-root", directory, "--update-baseline"]), 1)
                save.assert_not_called()


class DisplayNameTests(unittest.TestCase):
    def test_sentence_case_preserves_proper_names_and_acronyms(self):
        for name in ("Native NES search", "STB search and replay", "PostgreSQL fault search",
                     "VM execution and restore", "KVM deterministic replay", "Kani proofs",
                     "Public API compatibility", "Memory safety — ${{ matrix.name }}",
                     "Lint, build, and unit tests"):
            with self.subTest(name=name):
                self.assertTrue(LINTS._sentence_case_name(name))

    def test_generic_title_case_and_misspelled_acronyms_fail(self):
        for name in ("Gates", "Products", "Quality", "Report", "native NES search",
                     "Native NES Search", "Public Api Compatibility", "Public api compatibility",
                     "STB Search And Replay", "KANI proofs", "Checks / Memory safety", "${{ matrix.name }}"):
            with self.subTest(name=name):
                self.assertFalse(LINTS._sentence_case_name(name))

    def test_real_workflows_use_flat_names_and_reject_bad_job_names(self):
        root = SCRIPT.parent.parent
        for path, category in LINTS.PR_WORKFLOWS.items():
            with self.subTest(path=path):
                content = (root / path).read_text()
                parsed = LINTS._parse_workflow(root / path)
                self.assertEqual(parsed["name"], category)
                with tempfile.TemporaryDirectory() as directory:
                    copy_root = Path(directory)
                    target = copy_root / path
                    target.parent.mkdir(parents=True)
                    target.write_text(content)
                    self.assertFalse(LINTS.check_workflow_rules(copy_root, [path]))
                    first_name = next(iter(parsed["jobs"].values()))["name"]
                    for bad_name in ("Gates", "Choose Tests From Changed Files", "public api checks"):
                        target.write_text(content.replace("name: " + first_name, "name: " + bad_name, 1))
                        self.assertIn("ci-display-name", {v.rule for v in LINTS.check_workflow_rules(copy_root, [path])})
                    target.write_text(content.replace("name: " + category + "\n", "name: " + category + " / Products\n", 1))
                    self.assertIn("ci-display-name", {v.rule for v in LINTS.check_workflow_rules(copy_root, [path])})


class SmokeRoutingTests(unittest.TestCase):
    def setUp(self):
        self.path = ".github/workflows/product-smoke.yml"
        self.workflow = LINTS._parse_workflow(SCRIPT.parent.parent / self.path)

    def test_registered_smokes_pass(self):
        self.assertFalse(LINTS.check_pr_smoke_routing(self.path, self.workflow))

    def test_workflow_dispatcher_cannot_bypass_routing_by_renaming(self):
        original = (SCRIPT.parent.parent / self.path).read_text()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / self.path
            path.parent.mkdir(parents=True)
            path.write_text(original)
            self.assertFalse(LINTS.check_workflow_rules(root, [self.path]))
            path.write_text(original.replace("name: Smoke\n", "name: Checks\n"))
            rules = {v.rule for v in LINTS.check_workflow_rules(root, [self.path])}
            self.assertIn("ci-pr-smoke-routing", rules)
            self.assertIn("ci-pr-workflow-registration", rules)

    def test_new_checks_workflow_requires_registration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = ".github/workflows/extra-checks.yml"
            (root / path).parent.mkdir(parents=True)
            (root / path).write_text(workflow("bounded", "    timeout-minutes: 15\n    steps: []"))
            self.assertIn("ci-pr-workflow-registration", {v.rule for v in LINTS.check_workflow_rules(root, [path])})

    def test_separate_broad_trigger_is_rejected(self):
        self.assertTrue(LINTS.check_pr_smoke_routing(".github/workflows/another-smoke.yml", self.workflow))

    def test_bypassing_selector_or_adding_matrix_is_rejected(self):
        for mutation in ({"if": "always()"}, {"needs": []}, {"strategy": {"matrix": {"seed": [1, 2, 3]}}}):
            workflow = copy.deepcopy(self.workflow)
            workflow["jobs"]["native"].update(mutation)
            self.assertTrue(LINTS.check_pr_smoke_routing(self.path, workflow))

    def test_unregistered_consumer_is_rejected(self):
        self.workflow["jobs"]["extra"] = self.workflow["jobs"]["native"]
        self.assertTrue(LINTS.check_pr_smoke_routing(self.path, self.workflow))

    def test_unselected_smokes_cannot_run_setup_or_uploads(self):
        for index in range(2, len(self.workflow["jobs"]["native"]["steps"])):
            workflow = copy.deepcopy(self.workflow)
            del workflow["jobs"]["native"]["steps"][index]["if"]
            self.assertTrue(LINTS.check_pr_smoke_routing(self.path, workflow))
        for guard in ("always()", "steps.scope.outputs.enabled == 'true' || always()"):
            workflow = copy.deepcopy(self.workflow)
            workflow["jobs"]["native"]["steps"][-1]["if"] = guard
            self.assertTrue(LINTS.check_pr_smoke_routing(self.path, workflow))

    def test_scope_failures_and_shallow_diffs_cannot_be_ignored(self):
        for index, update in ((0, {"with": {"fetch-depth": 1}}), (1, {"continue-on-error": True}),
                              (1, {"if": "false"}), (1, {"with": {"kind": "smoke", "target": "stb"}})):
            workflow = copy.deepcopy(self.workflow)
            workflow["jobs"]["native"]["steps"][index].update(update)
            self.assertTrue(LINTS.check_pr_smoke_routing(self.path, workflow))

    def test_qualification_still_depends_on_selected_platform_evidence(self):
        self.workflow["jobs"]["guest-qualification"]["if"] = "false"
        self.assertTrue(LINTS.check_pr_smoke_routing(self.path, self.workflow))


class CheckRoutingTests(unittest.TestCase):
    def test_routing_is_inline_and_every_miri_target_is_present(self):
        for path in (".github/workflows/quality.yml", ".github/workflows/nightly.yml"):
            workflow = LINTS._parse_workflow(SCRIPT.parent.parent / path)
            self.assertFalse(LINTS.check_pr_check_routing(path, workflow))
            workflow["jobs"]["selection"] = {"steps": []}
            self.assertTrue(LINTS.check_pr_check_routing(path, workflow))
        path = ".github/workflows/nightly.yml"
        workflow = LINTS._parse_workflow(SCRIPT.parent.parent / path)
        workflow["jobs"]["miri-pr"]["strategy"]["matrix"]["name"].pop()
        self.assertTrue(LINTS.check_pr_check_routing(path, workflow))

    def test_check_steps_cannot_bypass_inline_selection(self):
        for path, job in ((".github/workflows/quality.yml", "kani"),
                          (".github/workflows/quality.yml", "public-api"),
                          (".github/workflows/nightly.yml", "miri-pr")):
            workflow = LINTS._parse_workflow(SCRIPT.parent.parent / path)
            for index in range(2, len(workflow["jobs"][job]["steps"])):
                modified = copy.deepcopy(workflow)
                del modified["jobs"][job]["steps"][index]["if"]
                self.assertTrue(LINTS.check_pr_check_routing(path, modified))


class NesWorkflowCoverageTests(unittest.TestCase):
    def test_registered_manifest_requires_tracked_owning_workflow(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "benchmarks/search/nightly.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({"cases": [{"id": "nova-full"}]}))
            owner = ".github/workflows/nova-nightly.yml"
            for files in ([], [owner]):
                with self.subTest(files=files):
                    violations = LINTS.check_workflow_rules(root, files)
                    self.assertIn("ci-nes-case-jobs", [v.rule for v in violations])
            (root / owner).parent.mkdir(parents=True)
            (root / owner).write_text("name: Benchmarks / NES\n")
            self.assertIn("ci-nes-case-jobs", [v.rule for v in LINTS.check_workflow_rules(root, [])])

    def check(self, cases, matrix, command="python3 benchmarks/search/eval.py run benchmarks/search/nightly.json --case '${{ matrix.case }}'", fail_fast=False, report_if="always()", needs="campaign"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "benchmarks/search/nightly.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({"cases": [{"id": case} for case in cases]}))
            data = {"jobs": {
                "campaign": {"strategy": {"fail-fast": fail_fast, "matrix": matrix}, "steps": [{"run": command}]},
                "report": {"if": report_if, "needs": needs},
            }}
            return LINTS.check_nes_job_coverage(root, ".github/workflows/nes.yml", data)

    def test_each_case_has_an_independent_job(self):
        self.assertFalse(self.check(["nova-full", "stb-hard"], {"case": ["nova-full", "stb-hard"]}))

    def test_monolithic_panel_is_rejected(self):
        self.assertTrue(self.check(["nova-full", "stb-hard"], {}, "python3 benchmarks/search/eval.py run benchmarks/search/nightly.json"))

    def test_new_game_requires_a_job(self):
        self.assertTrue(self.check(["nova-full", "stb-hard", "new-game"], {"case": ["nova-full", "stb-hard"]}))

    def test_duplicate_and_unknown_cases_are_rejected(self):
        for cases in (["nova", "nova"], ["nova", "unknown"]):
            self.assertTrue(self.check(["nova", "stb"], {"case": cases}))

    def test_extra_axis_cannot_multiply_case_jobs(self):
        self.assertTrue(self.check(["nova"], {"case": ["nova"], "repeat": [1, 2]}))

    def test_sibling_failures_do_not_cancel_cases_or_hide_report(self):
        for options in ({"fail_fast": True}, {"report_if": "success()"}, {"needs": []}):
            self.assertTrue(self.check(["nova"], {"case": ["nova"]}, **options))


if __name__ == "__main__":
    unittest.main()
