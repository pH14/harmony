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


class RepositoryVocabularyTests(unittest.TestCase):
    def test_words_and_identifier_components_are_rejected(self):
        word = LINTS.PROHIBITED_WORD
        for text in (word, word.upper(), word.title(), word[0].upper() + word[1:3] + word[3].upper(),
                     word + "s", "a_" + word, word + "_status",
                     word + "Ready", word + "sReady",
                     "Recovery" + word.title(), "Snapshot" + word.title() + "Status"):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "Makefile").write_text(text)
                violations = LINTS.check_repository_vocabulary(root, ["Makefile"])
                self.assertEqual([(v.rule, v.line) for v in violations], [(LINTS.VOCABULARY_RULE, 1)])

    def test_paths_are_checked_and_larger_words_and_binary_files_are_allowed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name = "test-" + LINTS.PROHIBITED_WORD + ".sh"
            (root / name).write_text("aggregate propagate delegate negate gateway")
            (root / "asset.bin").write_bytes(b"\0" + LINTS.PROHIBITED_WORD.encode())
            violations = LINTS.check_repository_vocabulary(root, [name, "asset.bin"])
            self.assertEqual([(v.path, v.line) for v in violations], [(name, 0)])

    def test_checker_is_in_scope_and_vocabulary_cannot_be_baselined(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = "scripts/custom-lints.py"
            (root / path).parent.mkdir()
            (root / path).write_text(LINTS.PROHIBITED_WORD)
            with mock.patch.object(LINTS, "tracked_files", return_value=[]), \
                 mock.patch.object(LINTS, "check_workflow_rules", return_value=[]), \
                 mock.patch.object(LINTS, "load_baseline", return_value={LINTS.VOCABULARY_RULE: [path + ":1"]}), \
                 mock.patch.object(LINTS, "save_baseline") as save, \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(LINTS.main(["--repo-root", directory]), 1)
                self.assertEqual(LINTS.main(["--repo-root", directory, "--update-baseline"]), 1)
                save.assert_not_called()


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


ROOT = SCRIPT.parent.parent
sys.path.insert(0, str(SCRIPT.parent))
import ci_contract


def workflow_text(name="Checks / Consonance", triggers=("pull_request", "push"), jobs=None) -> str:
    jobs = jobs if jobs is not None else "  guest-memory:\n    name: Guest Memory\n    timeout-minutes: 15\n    steps: []\n"
    trigger_block = "".join(f"  {trigger}:\n" for trigger in triggers)
    return f"name: {name}\non:\n{trigger_block}jobs:\n{jobs}"


def job_text(job_id: str, name: str, body: str) -> str:
    return f"  {job_id}:\n    name: {name}\n    runs-on: ubuntu-latest\n{body}\n"


def registered(name="Checks / Consonance", triggers=("pull_request", "push"), jobs=None,
               path=".github/workflows/example-checks.yml", owner="Consonance"):
    jobs = jobs if jobs is not None else (ci_contract.Job("Guest Memory", "pr", 15),)
    return ci_contract.Workflow(path=path, name=name, owner=owner, jobs=jobs, triggers=triggers)


class WorkflowFileTests(unittest.TestCase):
    def check(self, content: str, workflow=None) -> list[LINTS.Violation]:
        workflow = workflow if workflow is not None else registered()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / workflow.path
            path.parent.mkdir(parents=True)
            path.write_text(content)
            return LINTS.check_workflow_file(root, workflow.path, workflow)

    def test_a_registered_workflow_passes(self):
        self.assertFalse(self.check(workflow_text()))

    def test_the_repository_matches_its_registry(self):
        self.assertFalse(LINTS.check_workflow_rules(ROOT, list(ci_contract.registered_paths())))

    def test_name_must_match_the_registry(self):
        violations = self.check(workflow_text(name="Checks / Dissonance"))
        self.assertEqual([v.rule for v in violations], ["ci-workflow-name"])

    def test_triggers_must_match_the_registry(self):
        for triggers in (("pull_request",), ("pull_request", "push", "schedule")):
            with self.subTest(triggers=triggers):
                violations = self.check(workflow_text(triggers=triggers))
                self.assertIn("ci-workflow-triggers", [v.rule for v in violations])

    def test_parse_errors_fail_closed(self):
        for content in ("name: Checks / Consonance\njobs: [", "[]",
                        "name: Checks / Consonance\non: pull_request",
                        "name: Checks / Consonance\non: pull_request\njobs: {broken: null}",
                        "name: Checks / Consonance\non: pull_request\njobs: {broken: {steps: null}}"):
            with self.subTest(content=content):
                self.assertIn("ci-workflow-parse", [v.rule for v in self.check(content)])

    def test_duplicate_keys_are_rejected(self):
        content = workflow_text(jobs=job_text("guest-memory", "Guest Memory",
                                              "    timeout-minutes: 15\n    timeout-minutes: 90"))
        self.assertIn("ci-workflow-parse", [v.rule for v in self.check(content)])

    def test_missing_parser_is_an_error(self):
        with mock.patch.dict(sys.modules, {"yaml": None}):
            self.assertIn("ci-workflow-parse", [v.rule for v in self.check(workflow_text())])

    def test_workflows_must_be_registered_in_both_directions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            extra = ".github/workflows/extra-checks.yml"
            (root / extra).parent.mkdir(parents=True)
            (root / extra).write_text(workflow_text())
            with mock.patch.object(ci_contract, "registered_paths", lambda: (".github/workflows/absent.yml",)):
                rules = [v.rule for v in LINTS.check_workflow_rules(root, [extra])]
            self.assertEqual(rules.count("ci-workflow-registration"), 2)


class RegistryNameTests(unittest.TestCase):
    def rules(self, workflow) -> set[str]:
        return {v.rule for v in LINTS.check_registry_names([workflow])}

    def test_the_registry_passes_its_own_rules(self):
        self.assertFalse(LINTS.check_registry_names())

    def test_retired_and_unknown_categories_are_rejected(self):
        for name in ("Smoke / Consonance", "Nightly / Consonance", "Validation / Consonance",
                     "Acceptance / Consonance", "Quality / Consonance", "Testing / Consonance"):
            with self.subTest(name=name):
                self.assertIn("ci-workflow-name", self.rules(registered(name=name)))

    def test_display_names_are_title_case(self):
        for name in ("Checks / consonance", "Checks / Consonance / analysis", "Checks / PostgresQL"):
            with self.subTest(name=name):
                self.assertIn("ci-display-name", self.rules(registered(name=name)))
        for job in ("guest memory", "Guest memory", "Public Api"):
            with self.subTest(job=job):
                self.assertIn("ci-display-name",
                              self.rules(registered(jobs=(ci_contract.Job(job, "pr", 15),))))

    def test_display_names_never_name_a_trigger(self):
        for job in ("Nightly Search", "Scheduled Coverage", "Manual Qualification"):
            with self.subTest(job=job):
                self.assertIn("ci-display-name",
                              self.rules(registered(jobs=(ci_contract.Job(job, "pr", 15),))))

    def test_replicas_carry_a_label(self):
        self.assertIn("ci-display-name",
                      self.rules(registered(jobs=(ci_contract.Job("Snapshot Identity (1)", "pr", 15),))))
        self.assertNotIn("ci-display-name",
                         self.rules(registered(jobs=(ci_contract.Job("Snapshot Identity — Replica <N>", "pr", 15),))))


class JobContractTests(unittest.TestCase):
    def check(self, body: str, job=None, triggers=("pull_request", "push"), name="Guest Memory"):
        job = job if job is not None else ci_contract.Job("Guest Memory", "pr", 15)
        workflow = registered(triggers=triggers, jobs=(job,))
        content = workflow_text(triggers=triggers, jobs=job_text("job", name, body))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / workflow.path
            path.parent.mkdir(parents=True)
            path.write_text(content)
            return [v.rule for v in LINTS.check_workflow_file(root, workflow.path, workflow)]

    def test_bounded_jobs_declare_their_budget(self):
        self.assertFalse(self.check("    timeout-minutes: 15\n    steps: []"))
        self.assertEqual(self.check("    steps: []"), ["ci-pr-job-timeout"])

    def test_a_pull_request_job_cannot_exceed_the_bound(self):
        rules = self.check("    timeout-minutes: 16\n    steps: []",
                           job=ci_contract.Job("Guest Memory", "pr", 16))
        self.assertEqual(rules, ["ci-pr-job-timeout"])
        for minutes in (0, -1):
            with self.subTest(minutes=minutes):
                self.assertIn("ci-pr-job-timeout",
                              self.check(f"    timeout-minutes: {minutes}\n    steps: []",
                                         job=ci_contract.Job("Guest Memory", "pr", minutes)))

    def test_the_declared_budget_must_match_the_registry(self):
        self.assertEqual(self.check("    timeout-minutes: 10\n    steps: []"), ["ci-pr-job-timeout"])

    def test_a_bounded_job_cannot_be_guarded_away_from_pull_requests(self):
        rules = self.check("    if: github.event_name == 'schedule'\n    timeout-minutes: 15\n    steps: []")
        self.assertEqual(rules, ["ci-trigger-routing"])

    def test_scheduled_work_in_a_pull_request_workflow_needs_an_exception(self):
        guard = "    if: github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'\n"
        bare = ci_contract.Job("Coverage", "full", 30)
        self.assertEqual(self.check(guard + "    timeout-minutes: 30\n    steps: []",
                                    job=bare, name="Coverage"),
                         ["ci-trigger-exception"])
        excepted = ci_contract.Job("Coverage", "full", 30, exception="An instrumented build exceeds the bound.")
        self.assertFalse(self.check(guard + "    timeout-minutes: 30\n    steps: []",
                                    job=excepted, name="Coverage"))

    def test_scheduled_work_needs_no_exception_in_its_own_workflow(self):
        self.assertFalse(self.check("    timeout-minutes: 30\n    steps: []",
                                    job=ci_contract.Job("Coverage", "full", 30),
                                    triggers=("schedule", "workflow_dispatch"), name="Coverage"))

    def test_an_unguarded_long_job_reaches_pull_requests(self):
        job = ci_contract.Job("Coverage", "full", 30, exception="An instrumented build exceeds the bound.")
        for guard in ("", "    if: github.event_name != 'schedule'\n",
                      "    if: ${{ !(github.event_name == 'schedule') }}\n",
                      "    if: always() && (github.event_name == 'schedule' || github.event_name == 'pull_request')\n",
                      "    # ci-pr-job-timeout-exception: coverage -- approved elsewhere\n"):
            with self.subTest(guard=guard):
                rules = self.check(guard + "    timeout-minutes: 30\n    steps: []", job=job, name="Coverage")
                self.assertIn("ci-trigger-routing", rules)

    def test_nested_non_pull_request_branches_are_accepted(self):
        job = ci_contract.Job("Coverage", "full", 30, exception="An instrumented build exceeds the bound.")
        guard = "    if: always() && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')\n"
        self.assertFalse(self.check(guard + "    timeout-minutes: 30\n    steps: []", job=job, name="Coverage"))

    def test_every_pull_request_event_carries_the_bound(self):
        for event in ("pull_request", "pull_request_target", "merge_group"):
            with self.subTest(event=event):
                rules = self.check("    timeout-minutes: 90\n    steps: []",
                                   job=ci_contract.Job("Guest Memory", "pr", 90), triggers=(event,))
                self.assertIn("ci-pr-job-timeout", rules)

    def test_ci_errors_cannot_be_hidden_in_baseline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = ".github/workflows/example-checks.yml"
            (root / path).parent.mkdir(parents=True)
            (root / path).write_text(workflow_text(
                jobs=job_text("guest-memory", "Guest Memory", "    timeout-minutes: 90\n    steps: []")))
            workflow = registered(jobs=(ci_contract.Job("Guest Memory", "pr", 90),))
            with mock.patch.object(LINTS, "tracked_files", return_value=[path]), \
                 mock.patch.object(ci_contract, "registered_paths", lambda: (path,)), \
                 mock.patch.object(ci_contract, "by_path", lambda _: workflow), \
                 mock.patch.object(LINTS, "load_baseline", return_value={"ci-pr-job-timeout": [path + ":0"]}), \
                 mock.patch.object(LINTS, "save_baseline") as save, \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(LINTS.main(["--repo-root", directory]), 1)
                self.assertEqual(LINTS.main(["--repo-root", directory, "--update-baseline"]), 1)
                save.assert_not_called()


class DisplayNameTests(unittest.TestCase):
    def check(self, jobs: str, registry) -> list[str]:
        workflow = registered(jobs=registry)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / workflow.path
            path.parent.mkdir(parents=True)
            path.write_text(workflow_text(jobs=jobs))
            return [v.rule for v in LINTS.check_workflow_file(root, workflow.path, workflow)]

    def test_an_unregistered_job_is_rejected(self):
        rules = self.check(job_text("extra", "Guest Paging", "    timeout-minutes: 15\n    steps: []"),
                           (ci_contract.Job("Guest Memory", "pr", 15),))
        self.assertEqual(rules.count("ci-display-name"), 2)

    def test_a_registered_job_must_exist(self):
        rules = self.check(job_text("guest-memory", "Guest Memory", "    timeout-minutes: 15\n    steps: []"),
                           (ci_contract.Job("Guest Memory", "pr", 15), ci_contract.Job("CPU State", "pr", 15)))
        self.assertEqual(rules, ["ci-display-name"])

    def test_a_variant_matches_the_values_its_matrix_supplies(self):
        jobs = job_text("shards", "Mutation Testing — Shard ${{ matrix.shard }}/4",
                        "    timeout-minutes: 15\n    strategy:\n      matrix:\n        shard: [0, 1, 2, 3]\n    steps: []")
        self.assertFalse(self.check(jobs, (ci_contract.Job("Mutation Testing — Shard <N>/4", "pr", 15),)))

    def test_one_matrix_can_carry_several_registered_jobs(self):
        jobs = job_text("nested", "${{ matrix.name }}",
                        "    timeout-minutes: 15\n    strategy:\n      matrix:\n"
                        "        include:\n          - name: Docker\n          - name: K3s\n    steps: []")
        registry = (ci_contract.Job("Docker", "pr", 15), ci_contract.Job("K3s", "pr", 15))
        self.assertFalse(self.check(jobs, registry))
        self.assertEqual(self.check(jobs, registry[:1]).count("ci-display-name"), 1)

    def test_display_names_expand_only_what_the_file_declares(self):
        job = {"name": "Nova — Replica ${{ matrix.replica }}",
               "strategy": {"matrix": {"replica": [1, 2]}}}
        self.assertEqual(LINTS._job_display_names(job), ["Nova — Replica 1", "Nova — Replica 2"])
        dynamic = {"name": "${{ matrix.display_name }}",
                   "strategy": {"matrix": "${{ fromJSON(needs.manifest.outputs.matrix) }}"}}
        self.assertEqual(LINTS._job_display_names(dynamic), [LINTS.UNKNOWN_VALUE])


class ScopeRoutingTests(unittest.TestCase):
    def job(self, **changes) -> dict:
        job = {
            "name": "Nova",
            "timeout-minutes": 15,
            "steps": [
                {"uses": "actions/checkout@v4", "with": {"fetch-depth": 0, "filter": "blob:none"}},
                {"uses": "./.github/actions/ci-scope", "id": "scope", "with": {"kind": "harmony_nes"}},
                {"run": "harmony search", "if": "${{ steps.scope.outputs.enabled == 'true' }}"},
            ],
        }
        job.update(changes)
        return job

    def check(self, job, registry=None) -> list[str]:
        registry = registry if registry is not None else ci_contract.Job("Nova", "pr", 15, scope="harmony_nes")
        return [v.rule for v in LINTS.check_job_scope(".github/workflows/example.yml", "nova", job, registry)]

    def test_a_selecting_job_passes(self):
        self.assertFalse(self.check(self.job()))

    def test_selection_must_use_the_registered_kind(self):
        job = self.job()
        job["steps"][1]["with"] = {"kind": "dissonance_nes"}
        self.assertEqual(self.check(job), ["ci-scope-routing"])

    def test_a_registered_scope_must_be_selected(self):
        job = self.job()
        del job["steps"][1]
        self.assertEqual(self.check(job), ["ci-scope-routing"])

    def test_an_unregistered_job_cannot_select(self):
        self.assertEqual(self.check(self.job(), ci_contract.Job("Nova", "pr", 15)), ["ci-scope-routing"])

    def test_the_selector_itself_cannot_be_skipped(self):
        for change in ({"if": "false"}, {"continue-on-error": True}, {"id": "other"}):
            with self.subTest(change=change):
                job = self.job()
                job["steps"][1].update(change)
                self.assertEqual(self.check(job), ["ci-scope-routing"])

    def test_selection_needs_the_complete_diff(self):
        job = self.job()
        job["steps"][0]["with"] = {"fetch-depth": 1}
        self.assertEqual(self.check(job), ["ci-scope-routing"])

    def test_selected_work_must_require_its_selection(self):
        for guard in (None, "always()", "steps.scope.outputs.enabled == 'true' || always()"):
            with self.subTest(guard=guard):
                job = self.job()
                if guard is None:
                    del job["steps"][2]["if"]
                else:
                    job["steps"][2]["if"] = guard
                self.assertEqual(self.check(job), ["ci-scope-routing"])


class NesCompositionTests(unittest.TestCase):
    def test_both_compositions_keep_a_check_and_a_benchmark(self):
        paths = {ci_contract.by_name(entry[role]).path
                 for entry in ci_contract.NES_COMPOSITIONS.values()
                 for role in ("checks", "benchmarks")}
        self.assertEqual(len(paths), 4)
        self.assertFalse(LINTS.check_nes_compositions(ROOT, paths))
        for dropped in sorted(paths):
            with self.subTest(dropped=dropped):
                violations = LINTS.check_nes_compositions(ROOT, paths - {dropped})
                self.assertEqual([v.rule for v in violations], ["ci-nes-compositions"])

    def test_a_deleted_composition_is_reported(self):
        with mock.patch.object(ci_contract, "NES_COMPOSITIONS", {}):
            violations = LINTS.check_nes_compositions(ROOT, set())
        self.assertEqual([v.rule for v in violations], ["ci-nes-compositions"] * 2)


class NesCaseCoverageTests(unittest.TestCase):
    PATH = ci_contract.by_name(
        ci_contract.NES_COMPOSITIONS["Dissonance Workloads"]["benchmarks"]).path
    COMMAND = ("python3 benchmarks/search/eval.py run benchmarks/search/nightly.json "
               "--case '${{ matrix.case }}'")

    def check(self, cases, include, command=COMMAND, fail_fast=False,
              report_if="always()", needs=("campaign",), extra_axis=None):
        matrix = {"include": include}
        if extra_axis is not None:
            matrix.update(extra_axis)
        data = {
            "name": "Benchmarks / Dissonance Workloads / NES",
            "on": {"schedule": None},
            "jobs": {
                "campaign": {"name": "${{ matrix.name }}",
                             "strategy": {"fail-fast": fail_fast, "matrix": matrix},
                             "steps": [{"run": command}]},
                "report": {"name": "Results", "if": report_if, "needs": list(needs), "steps": []},
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "benchmarks/search/nightly.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({"cases": [{"id": case} for case in cases]}))
            path = root / self.PATH
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps(data))
            return [v.rule for v in LINTS.check_nes_case_coverage(root, {self.PATH})]

    def entries(self, *cases):
        return [{"name": case.replace("-", " ").title(), "case": case} for case in cases]

    def test_the_repository_covers_its_manifest(self):
        self.assertFalse(LINTS.check_nes_case_coverage(ROOT, {self.PATH}))

    def test_each_case_has_an_independent_job(self):
        self.assertFalse(self.check(["nova-full", "stb-hard"], self.entries("nova-full", "stb-hard")))

    def test_the_manifest_requires_its_tracked_owning_workflow(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "benchmarks/search/nightly.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({"cases": [{"id": "nova-full"}]}))
            violations = LINTS.check_nes_case_coverage(root, set())
        self.assertEqual([v.rule for v in violations], ["ci-nes-case-jobs"])

    def test_a_monolithic_panel_is_rejected(self):
        rules = self.check(["nova-full", "stb-hard"], self.entries("nova-full", "stb-hard"),
                           command="python3 benchmarks/search/eval.py run benchmarks/search/nightly.json")
        self.assertEqual(rules, ["ci-nes-case-jobs"])

    def test_a_new_case_requires_a_job(self):
        self.assertEqual(self.check(["nova-full", "stb-hard", "new-game"],
                                    self.entries("nova-full", "stb-hard")), ["ci-nes-case-jobs"])

    def test_duplicate_and_unknown_cases_are_rejected(self):
        for cases in (("nova", "nova"), ("nova", "unknown")):
            with self.subTest(cases=cases):
                self.assertEqual(self.check(["nova", "stb"], self.entries(*cases)), ["ci-nes-case-jobs"])

    def test_an_extra_axis_cannot_multiply_case_jobs(self):
        self.assertEqual(self.check(["nova"], self.entries("nova"), extra_axis={"repeat": [1, 2]}),
                         ["ci-nes-case-jobs"])

    def test_every_matrix_entry_names_its_scenario(self):
        self.assertEqual(self.check(["nova"], [{"case": "nova"}]), ["ci-nes-case-jobs"])

    def test_sibling_failures_do_not_cancel_cases_or_hide_results(self):
        for options in ({"fail_fast": True}, {"report_if": "success()"}, {"needs": ()}):
            with self.subTest(options=options):
                self.assertEqual(self.check(["nova"], self.entries("nova"), **options),
                                 ["ci-nes-case-jobs"])


class MiriMatrixTests(unittest.TestCase):
    def paths(self):
        return {ci_contract.by_name(name).path
                for name in ci_contract.MIRI_ANALYSIS_WORKFLOWS.values()}

    def test_every_analysis_workflow_lists_the_targets_it_owns(self):
        self.assertFalse(LINTS.check_miri_matrices(ROOT, self.paths()))

    def test_a_dropped_target_is_reported(self):
        listed = LINTS._miri_matrix_names

        def drop(data, want, suffix):
            return listed(data, want)[1:] if want == suffix else listed(data, want)

        for suffix in ("", " (Whole Crate)"):
            with self.subTest(suffix=suffix):
                with mock.patch.object(LINTS, "_miri_matrix_names",
                                       side_effect=lambda data, want, suffix=suffix: drop(data, want, suffix)):
                    violations = LINTS.check_miri_matrices(ROOT, self.paths())
                self.assertTrue(violations)
                self.assertEqual({v.rule for v in violations}, {"ci-miri-coverage"})

    def test_an_unowned_target_is_reported(self):
        with mock.patch.object(ci_contract, "MIRI_OWNERS", {}):
            violations = LINTS.check_miri_matrices(ROOT, self.paths())
        self.assertIn("ci-miri-coverage", {v.rule for v in violations})


if __name__ == "__main__":
    unittest.main()
