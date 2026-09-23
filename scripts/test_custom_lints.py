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

    def test_checker_is_in_scope_of_the_vocabulary_rule(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = "scripts/custom-lints.py"
            (root / path).parent.mkdir()
            (root / path).write_text(LINTS.PROHIBITED_WORD)
            with mock.patch.object(LINTS, "tracked_files", return_value=[]), \
                 mock.patch.object(LINTS, "check_workflow_rules", return_value=[]), \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(LINTS.main(["--repo-root", directory]), 1)


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


class StorageBugVocabularyTests(unittest.TestCase):
    def test_fault_search_code_cannot_name_the_storage_bug(self) -> None:
        rules = {rule.name: rule for rule in LINTS.RULES}
        fault = rules["fault-search-no-storage-bug-vocabulary"]
        searcher = rules["searcher-no-storage-bug-vocabulary"]
        for path in ("workloads/faults/runtime/fault_runtime.c",
                     "workloads/fault-policy/src/lib.rs",
                     "consonance/harmony-linux/supervisor/src/main.rs",
                     "consonance/harmony-linux/linux/patches/common/0001-harmony-character-device.patch"):
            self.assertTrue(fault.applies(path), path)
        self.assertFalse(fault.applies("workloads/faults/README.md"))
        self.assertFalse(fault.applies("workloads/bugs/historical/sqlite-wal-reset/image/bundle"))
        self.assertTrue(searcher.applies("dissonance/searcher/src/search/campaign.rs"))
        self.assertTrue(searcher.applies("workloads/faults/src/archive.rs"))
        self.assertFalse(fault.applies("workloads/faults/src/archive.rs"))
        for text in ("SQLite", "sqlite3_step", "the WAL", "wal_index", "Backfilled", "checkpoint"):
            self.assertIsNotNone(fault.pattern.search(text), text)
        for text in ("walk", "wall_minutes", "tables.checkpoint()"):
            self.assertIsNone(searcher.pattern.search(text), text)
        for text in ("run_campaign_checkpointed", "SNAPSHOT_CHECKPOINT_FORMAT", "_checkpoint"):
            self.assertIsNone(fault.pattern.search(text), text)
        self.assertIsNotNone(searcher.pattern.search("sqlite"))


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

    def test_each_push_gets_its_own_concurrency_group(self):
        key = ci_contract.PUSH_CONCURRENCY_KEY
        jobs = "  guest-memory:\n    name: Guest Memory\n    timeout-minutes: 15\n    steps: []\n"
        for block, expected in (
            (f"concurrency:\n  group: checks-{key}\n  cancel-in-progress: true\n", []),
            ("concurrency:\n  group: checks-${{ github.ref }}\n  cancel-in-progress: true\n",
             ["ci-push-concurrency"]),
            ("concurrency:\n  group: checks-${{ github.ref }}\n  cancel-in-progress: false\n",
             ["ci-push-concurrency"]),
            ("concurrency: checks-${{ github.ref }}\n", ["ci-push-concurrency"]),
        ):
            with self.subTest(block=block):
                content = workflow_text().replace("jobs:\n", block + "jobs:\n")
                self.assertEqual([v.rule for v in self.check(content)], expected)
        job_group = jobs + "    concurrency: memory-${{ github.ref }}\n"
        self.assertEqual([v.rule for v in self.check(workflow_text(jobs=job_group))],
                         ["ci-push-concurrency"])
        pr_only = workflow_text(triggers=("pull_request",)).replace(
            "jobs:\n", "concurrency:\n  group: checks-${{ github.ref }}\n  cancel-in-progress: true\njobs:\n")
        self.assertFalse(self.check(pr_only, registered(triggers=("pull_request",))))

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


class RegistryStructureTests(unittest.TestCase):
    def rules(self, *workflows) -> list[str]:
        return [v.rule for v in LINTS.check_registry_names(workflows)]

    def test_two_workflows_cannot_share_a_name_or_a_path(self):
        rules = self.rules(registered(name="Checks / Consonance"),
                           registered(name="Checks / Consonance"))
        self.assertEqual(rules.count("ci-workflow-registration"), 2)

    def test_a_bare_category_names_no_owner(self):
        for name in ("Checks", "Checks / Everything", "Checks / Consonance / Guest / Memory"):
            with self.subTest(name=name):
                self.assertIn("ci-workflow-name", self.rules(registered(name=name)))

    def test_a_job_cannot_repeat_the_workflow_hierarchy(self):
        for job in ("Consonance / Guest Memory", "Checks / Guest Memory"):
            with self.subTest(job=job):
                self.assertIn("ci-display-name",
                              self.rules(registered(jobs=(ci_contract.Job(job, "pr", 15),))))

    def test_a_generic_job_name_is_rejected_and_a_contextual_one_is_kept(self):
        for job in ("Unit Tests", "Tests", "Build", "Verify", "unit tests"):
            with self.subTest(job=job):
                self.assertIn("ci-display-name",
                              self.rules(registered(jobs=(ci_contract.Job(job, "pr", 15),))))
        for job in ("Nova", "Coverage", "Results", "Guest Memory"):
            with self.subTest(job=job):
                self.assertNotIn("ci-display-name",
                                 self.rules(registered(jobs=(ci_contract.Job(job, "pr", 15),))))

    def test_a_job_cannot_be_registered_twice_in_one_workflow(self):
        jobs = (ci_contract.Job("Guest Memory", "pr", 15), ci_contract.Job("Guest Memory", "pr", 15))
        self.assertIn("ci-display-name", self.rules(registered(jobs=jobs)))

    def test_a_variant_uses_the_registered_separator(self):
        for job in ("Nova - <N>", "Nova: <N>", "Nova <N>"):
            with self.subTest(job=job):
                self.assertIn("ci-display-name",
                              self.rules(registered(jobs=(ci_contract.Job(job, "pr", 15),))))

    def test_analysis_belongs_to_the_component_that_owns_the_code(self):
        for job in ("Coverage", "Miri — <Crate>", "Mutation Testing — Shard <N>/4", "Proofs"):
            with self.subTest(job=job):
                bounded = registered(name="Checks / Consonance",
                                     jobs=(ci_contract.Job(job, "full", 30, exception="long"),))
                self.assertIn("ci-analysis-grouping", self.rules(bounded))
                analysis = registered(name="Checks / Consonance / Analysis",
                                      jobs=(ci_contract.Job(job, "full", 30, exception="long"),))
                self.assertNotIn("ci-analysis-grouping", self.rules(analysis))

    def test_a_registered_budget_stays_inside_the_bound(self):
        self.assertIn("ci-pr-job-timeout",
                      self.rules(registered(jobs=(ci_contract.Job("Guest Memory", "pr", 45),))))
        self.assertIn("ci-trigger-exception",
                      self.rules(registered(jobs=(ci_contract.Job("Coverage", "full", 45),))))
        self.assertIn("ci-trigger-routing",
                      self.rules(registered(jobs=(ci_contract.Job("Guest Memory", "weekly", 15),))))

    def test_a_workflow_without_a_job_owns_nothing(self):
        self.assertIn("ci-display-name", self.rules(registered(jobs=())))


class HostCompatibilityTests(unittest.TestCase):
    def workflow(self, jobs):
        return registered(name=LINTS.HOST_COMPATIBILITY_WORKFLOW, owner="Harmony Host Compatibility",
                          path=".github/workflows/harmony-host-compatibility.yml", jobs=jobs)

    def test_the_repository_checks_every_supported_host(self):
        self.assertFalse([v for v in LINTS.check_registry_names()
                          if v.rule == "ci-host-compatibility"])

    def test_a_missing_composition_is_reported(self):
        rules = [v.rule for v in LINTS.check_host_compatibility([registered()])]
        self.assertEqual(rules, ["ci-host-compatibility"])

    def test_a_dropped_or_scheduled_host_is_reported(self):
        jobs = (ci_contract.Job("macOS Arm64", "pr", 15),)
        self.assertEqual([v.rule for v in LINTS.check_host_compatibility([self.workflow(jobs)])],
                         ["ci-host-compatibility"])
        jobs += (ci_contract.Job("Linux Arm64", "full", 45),)
        self.assertEqual([v.rule for v in LINTS.check_host_compatibility([self.workflow(jobs)])],
                         ["ci-host-compatibility"])
        jobs = (ci_contract.Job("macOS Arm64", "pr", 15), ci_contract.Job("Linux Arm64", "pr", 15))
        self.assertFalse(LINTS.check_host_compatibility([self.workflow(jobs)]))


class HistoricalArmTests(unittest.TestCase):
    def case(self, **changes) -> dict:
        case = {"software": "PostgreSQL", "workload": {"version": "14.3"},
                "ci": {"display_name": "PostgreSQL Index Corruption"}}
        case.update(changes)
        return case

    def check(self, case=None, workflows=None) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files, registered_paths = set(), set()
            if case is not None:
                rel = f"{ci_contract.HISTORICAL_CASE_ROOT}/postgres-cic-corruption/case.json"
                (root / rel).parent.mkdir(parents=True)
                (root / rel).write_text(json.dumps(case))
                files.add(rel)
            for rel, content in (workflows or {}).items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text(content)
                registered_paths.add(rel)
            return [v.rule for v in LINTS.check_historical_arms(root, files, registered_paths)]

    def test_the_repository_declares_no_comparison_arm(self):
        self.assertFalse([v for v in LINTS.check_workflow_rules(ROOT, LINTS.tracked_files(ROOT))
                          if v.rule == "ci-historical-arms"])

    def test_a_current_case_passes(self):
        self.assertFalse(self.check(case=self.case()))

    def test_a_reintroduced_arm_is_rejected(self):
        for key in ci_contract.FORBIDDEN_HISTORICAL_KEYS:
            with self.subTest(key=key):
                self.assertEqual(self.check(case=self.case(**{key: ["current", "fixed"]})),
                                 ["ci-historical-arms"])
        nested = self.case(search={"replay_arms": ["current"]})
        self.assertEqual(self.check(case=nested), ["ci-historical-arms"])

    def test_a_case_names_the_scenario_its_job_displays(self):
        self.assertEqual(self.check(case={"software": "PostgreSQL"}), ["ci-historical-arms"])

    def test_a_workflow_cannot_carry_an_execution_arm_axis(self):
        rel = ".github/workflows/example-checks.yml"
        jobs = job_text("scenario", "${{ matrix.display_name }}",
                        "    timeout-minutes: 15\n    strategy:\n      matrix:\n"
                        "        arm: [current, fixed]\n    steps: []")
        self.assertEqual(self.check(case=self.case(), workflows={rel: workflow_text(jobs=jobs)}),
                         ["ci-historical-arms"])


class PinnedSeedOutcomeTests(unittest.TestCase):
    def check(self, rel: str, content: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(content)
            return [v.rule for v in LINTS.check_pinned_seed_outcomes(root, [rel])]

    def test_the_repository_pins_no_seed_outcome(self):
        self.assertFalse(LINTS.check_pinned_seed_outcomes(ROOT, LINTS.tracked_files(ROOT)))

    def test_a_literal_seed_in_an_expected_pattern_is_rejected(self):
        rel = ".github/workflows/example-benchmarks.yml"
        content = ("          grep -Eq 'ORACLE_OK actions=[0-9]+ seed=0000000001352825' "
                   "report.txt\n")
        self.assertEqual(self.check(rel, content), ["ci-pinned-seed-outcome"])

    def test_a_seed_shape_is_accepted(self):
        rel = ".github/workflows/example-benchmarks.yml"
        content = "          grep -Eq 'ORACLE_OK seed=[0-9a-f]{16}' report.txt\n"
        self.assertFalse(self.check(rel, content))

    def test_a_seed_the_job_supplies_as_input_is_accepted(self):
        rel = ".github/workflows/example-benchmarks.yml"
        content = "          tools/campaign --seed 20260901 --executions 10\n"
        self.assertFalse(self.check(rel, content))

    def test_a_one_digit_seed_in_an_expected_pattern_is_rejected(self):
        rel = ".github/workflows/example-benchmarks.yml"
        content = "          grep -Eq 'OK seed=1' report.txt\n"
        self.assertEqual(self.check(rel, content), ["ci-pinned-seed-outcome"])

    def test_a_yaml_workflow_is_checked(self):
        rel = ".github/workflows/example-benchmarks.yaml"
        content = "          grep -Eq 'OK seed=1352825' report.txt\n"
        self.assertEqual(self.check(rel, content), ["ci-pinned-seed-outcome"])

    def test_a_seed_input_after_an_unrelated_check_is_accepted(self):
        rel = "scripts/example.sh"
        content = "grep -q READY report.txt && harmony search --seed=123\n"
        self.assertFalse(self.check(rel, content))

    def test_a_pinned_seed_in_a_quoted_alternation_is_rejected(self):
        rel = "scripts/example.sh"
        content = "grep -Eq 'seed=[0-9a-f]{16}|OK seed=77' report.txt\n"
        self.assertEqual(self.check(rel, content), ["ci-pinned-seed-outcome"])

    def test_a_pinned_seed_after_an_escaped_bar_is_rejected(self):
        rel = "scripts/example.sh"
        content = "grep -Eq foo\\|seed=77 report.txt\n"
        self.assertEqual(self.check(rel, content), ["ci-pinned-seed-outcome"])

    def test_a_pinned_seed_across_a_line_continuation_is_rejected(self):
        rel = "scripts/example.sh"
        content = ("grep -Eq \\\n"
                   "    'ORACLE_OK seed=1352825' \\\n"
                   "    report.txt\n")
        self.assertEqual(self.check(rel, content), ["ci-pinned-seed-outcome"])

    def test_a_seed_outside_an_assertion_is_accepted(self):
        rel = ".github/workflows/example-benchmarks.yml"
        content = "          PACKAGE_SEED: \"20260901\"\n"
        self.assertFalse(self.check(rel, content))


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

    def test_a_short_budget_cannot_hide_a_full_capability_search(self):
        for command in ci_contract.FULL_SEARCH_COMMANDS:
            with self.subTest(command=command):
                body = f"    timeout-minutes: 15\n    steps:\n      - run: {command} --out run\n"
                self.assertEqual(self.check(body), ["ci-trigger-routing"])

    def test_a_scheduled_job_may_run_a_full_capability_search(self):
        job = ci_contract.Job("Coverage", "full", 210)
        body = ("    timeout-minutes: 210\n    steps:\n"
                "      - run: benchmarks/search/eval.py run suite.json --out run\n")
        self.assertFalse(self.check(body, job=job, triggers=("schedule", "workflow_dispatch"),
                                    name="Coverage"))

    def test_a_job_that_runs_ignored_tests_registers_them(self):
        step = ("    timeout-minutes: 15\n    steps:\n"
                "      - run: cargo test -p vmm-backend --test kvm_smoke serviced_io -- --ignored --exact\n")
        self.assertEqual(self.check(step), ["ci-ignored-tests"])
        exact = ci_contract.Job("Guest Memory", "pr", 15,
                                ignored_tests=("vmm-backend::kvm_smoke serviced_io",))
        self.assertFalse(self.check(step, job=exact))
        whole = ci_contract.Job("Guest Memory", "pr", 15, ignored_tests=("vmm-backend::kvm_smoke *",))
        self.assertFalse(self.check(step, job=whole))

    def test_a_job_runs_the_ignored_tests_it_registers(self):
        job = ci_contract.Job("Guest Memory", "pr", 15,
                              ignored_tests=("vmm-backend::kvm_smoke serviced_mmio",))
        for body in ("    timeout-minutes: 15\n    steps: []",
                     "    timeout-minutes: 15\n    steps:\n"
                     "      - run: cargo test -p vmm-backend --test kvm_smoke serviced_io -- --ignored\n",
                     "    timeout-minutes: 15\n    steps:\n"
                     "      - run: cargo test -p vmm-backend --test kvm_smoke_extra serviced_mmio -- --ignored\n"):
            with self.subTest(body=body):
                self.assertEqual(self.check(body, job=job), ["ci-ignored-tests"])

    def test_a_ci_violation_fails_the_run(self):
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
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(LINTS.main(["--repo-root", directory]), 1)


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

    def test_two_jobs_cannot_display_the_same_name(self):
        jobs = (job_text("first", "Guest Memory", "    timeout-minutes: 15\n    steps: []")
                + job_text("second", "Guest Memory", "    timeout-minutes: 15\n    steps: []"))
        rules = self.check(jobs, (ci_contract.Job("Guest Memory", "pr", 15),))
        self.assertEqual(rules.count("ci-display-name"), 1)

    def test_a_matrix_cannot_repeat_one_display_name(self):
        jobs = job_text("replicas", "Snapshot Identity",
                        "    timeout-minutes: 15\n    strategy:\n      matrix:\n"
                        "        replica: [1, 2]\n    steps: []")
        rules = self.check(jobs, (ci_contract.Job("Snapshot Identity", "pr", 15),))
        self.assertEqual(rules, ["ci-display-name"])
        labelled = job_text("replicas", "Snapshot Identity — Replica ${{ matrix.replica }}",
                            "    timeout-minutes: 15\n    strategy:\n      matrix:\n"
                            "        replica: [1, 2]\n    steps: []")
        self.assertFalse(self.check(labelled, (ci_contract.Job("Snapshot Identity — Replica <N>", "pr", 15),)))

    def test_an_include_entry_refines_the_combination_it_matches(self):
        job = {"name": "Nova — Replica ${{ matrix.replica }}",
               "strategy": {"matrix": {"replica": [1, 2],
                                       "include": [{"replica": 1, "extra": True}]}}}
        self.assertEqual(LINTS._job_display_names(job), ["Nova — Replica 1", "Nova — Replica 2"])
        self.assertEqual(LINTS._matrix_rows(job),
                         [{"replica": 1, "extra": True}, {"replica": 2}])

    def test_an_excluded_combination_is_not_displayed(self):
        job = {"name": "Nova — Replica ${{ matrix.replica }}",
               "strategy": {"matrix": {"replica": [1, 2], "exclude": [{"replica": 2}]}}}
        self.assertEqual(LINTS._job_display_names(job), ["Nova — Replica 1"])

    def test_an_include_entry_the_axes_cannot_hold_becomes_its_own_job(self):
        job = {"name": "Nova — Replica ${{ matrix.replica }}",
               "strategy": {"matrix": {"replica": [1], "include": [{"replica": 2}]}}}
        self.assertEqual(LINTS._job_display_names(job), ["Nova — Replica 1", "Nova — Replica 2"])

    def test_two_include_entries_the_axes_cannot_hold_stay_separate(self):
        job = {"name": "Nova — Replica ${{ matrix.replica }}",
               "strategy": {"matrix": {"replica": [1], "include": [
                   {"replica": 2, "extra": "first"}, {"replica": 2, "extra": "second"}]}}}
        self.assertEqual(LINTS._matrix_rows(job),
                         [{"replica": 1}, {"replica": 2, "extra": "first"},
                          {"replica": 2, "extra": "second"}])

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

    def test_every_composition_runs_the_backend_it_is_registered_with(self):
        for composition, entry in ci_contract.NES_COMPOSITIONS.items():
            for role in ("checks", "benchmarks"):
                with self.subTest(composition=composition, role=role):
                    workflow = ci_contract.by_name(entry[role])
                    self.assertFalse(LINTS.check_nes_backend(ROOT, composition, entry, role, workflow))

    def test_a_vm_composition_running_only_native_is_reported(self):
        entry = ci_contract.NES_COMPOSITIONS["Harmony Workloads"]
        workflow = ci_contract.by_name(entry["checks"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / workflow.path).parent.mkdir(parents=True)
            (root / workflow.path).write_text("run: harmony search --backend native game.nes\n")
            violations = LINTS.check_nes_backend(root, "Harmony Workloads", entry, "checks", workflow)
        self.assertEqual([v.rule for v in violations], ["ci-nes-compositions"])
        self.assertIn("never runs the consonance backend", violations[0].text)

    def test_an_unregistered_backend_is_reported(self):
        violations = LINTS.check_nes_backend(ROOT, "Harmony Workloads", {"backend": "invented"},
                                             "checks", ci_contract.by_name("Checks / Harmony Workloads / NES"))
        self.assertEqual([v.rule for v in violations], ["ci-nes-compositions"])


class NesMediaTests(unittest.TestCase):
    def composition(self, role):
        entry = ci_contract.NES_COMPOSITIONS["Dissonance Workloads"]
        return ci_contract.by_name(entry[role])

    def test_every_registered_composition_films_its_scenarios(self):
        for composition, entry in ci_contract.NES_COMPOSITIONS.items():
            for role in ("checks", "benchmarks"):
                with self.subTest(composition=composition, role=role):
                    workflow = ci_contract.by_name(entry[role])
                    self.assertFalse(LINTS.check_nes_media(ROOT, composition, role, workflow))

    def test_a_composition_that_films_nothing_is_reported(self):
        workflow = self.composition("checks")
        stripped = workflow._replace(jobs=tuple(job._replace(media=()) for job in workflow.jobs))
        violations = LINTS.check_nes_media(ROOT, "Dissonance Workloads", "checks", stripped)
        self.assertIn("ci-nes-media", [v.rule for v in violations])
        self.assertTrue(any("captures video with game audio" in v.text for v in violations))

    def test_filming_only_outside_the_bounded_budget_is_reported(self):
        workflow = self.composition("checks")
        full = workflow._replace(jobs=tuple(job._replace(trigger="full") for job in workflow.jobs))
        self.assertTrue(any("no pr job" in v.text
                            for v in LINTS.check_nes_media(ROOT, "Dissonance Workloads", "checks", full)))

    def plant(self, root, workflow, steps):
        """A file whose jobs carry the registered names and the given steps."""
        import ci_contract as contract

        for capture in contract.CAPTURE_ACTIONS + contract.CAPTURE_SCRIPTS:
            target = root / capture
            if capture.endswith((".rs", ".py")):
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("")
            else:
                target.mkdir(parents=True, exist_ok=True)
                (target / "action.yml").write_text("runs:\n  using: composite\n  steps: []\n")
        body = "".join(
            f"  job{index}:\n    name: {LINTS.VARIANT_RE.sub('One', job.name)}\n"
            f"    steps:\n{steps}"
            for index, job in enumerate(workflow.jobs))
        path = root / workflow.path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("name: x\non:\n  pull_request:\njobs:\n" + body)

    def test_a_capture_step_the_job_can_skip_does_not_count(self):
        workflow = self.composition("checks")
        steps = ("      - run: nes-film --out film\n        if: false\n"
                 "      - uses: ./.github/actions/stb-evaluation\n"
                 "      - run: python3 scripts/verify-nes-films.py film/film.json\n")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, workflow, steps)
            violations = LINTS.check_nes_media(root, "Dissonance Workloads", "checks", workflow)
        self.assertTrue(any("no step it always runs captures with it" in v.text
                            for v in violations), violations)

    def test_a_check_disabled_inside_a_composite_action_does_not_count(self):
        workflow = self.composition("checks")
        steps = ("      - run: nes-film --out film\n"
                 "      - uses: ./.github/actions/stb-evaluation\n"
                 "      - uses: ./.github/actions/nes-film\n")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, workflow, steps)
            (root / ".github/actions/nes-film/action.yml").write_text(
                "runs:\n  using: composite\n  steps:\n"
                "    - run: python3 scripts/verify-nes-films.py --index films.json\n"
                "      if: false\n")
            violations = LINTS.check_nes_media(root, "Dissonance Workloads", "checks", workflow)
        self.assertTrue(any("real audio stream" in v.text for v in violations), violations)

    def test_a_capture_step_the_change_selection_guards_still_counts(self):
        workflow = self.composition("checks")
        steps = ("      - run: nes-film --out film\n"
                 "        if: steps.scope.outputs.enabled == 'true'\n"
                 "      - uses: ./.github/actions/stb-evaluation\n"
                 "      - run: python3 scripts/verify-nes-films.py film/film.json\n")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, workflow, steps)
            self.assertFalse(LINTS.check_nes_media(root, "Dissonance Workloads", "checks", workflow))

    def test_an_unregistered_or_missing_capture_is_reported(self):
        workflow = self.composition("benchmarks")
        for capture, expected in [("scripts/invented-capture.py", "unregistered capture"),
                                  (".github/actions/stb-evaluation",
                                   "no step it always runs captures with it")]:
            with self.subTest(capture=capture):
                jobs = tuple(job._replace(media=(capture,)) if job.media else job
                             for job in workflow.jobs)
                violations = LINTS.check_nes_media(ROOT, "Dissonance Workloads", "benchmarks",
                                                   workflow._replace(jobs=jobs))
                self.assertTrue(any(expected in v.text for v in violations), violations)

    def test_media_that_is_never_checked_for_audio_is_reported(self):
        workflow = self.composition("checks")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, workflow, "      - run: nes-film --out film\n")
            violations = LINTS.check_nes_media(root, "Dissonance Workloads", "checks", workflow)
        self.assertTrue(any("real audio stream" in v.text for v in violations), violations)

    def test_a_longer_name_containing_the_capture_does_not_satisfy_it(self):
        self.assertFalse(LINTS.names_capture(
            "python3 scripts/verify-nes-films.py film/film.json",
            "workloads/nes/src/bin/nes-film.rs"))
        self.assertTrue(LINTS.names_capture(
            "workloads/nes/target/release/nes-film --game nova",
            "workloads/nes/src/bin/nes-film.rs"))

    def test_the_repository_registers_a_capture_for_every_filming_job(self):
        self.assertTrue(ci_contract.MEDIA_REQUIRED)
        for (workflow, job), media in ci_contract.MEDIA_REQUIRED.items():
            with self.subTest(workflow=workflow, job=job):
                self.assertTrue(media)
                for capture in media:
                    self.assertTrue((ROOT / capture).exists(), capture)


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
