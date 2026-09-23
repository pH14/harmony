#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The registry describes CI that exists, owns every check, and stays consistent."""

import json
import re
import unittest
from pathlib import Path
from unittest import mock

import ci_contract
import miri_scope
from ci_scope import SCENARIOS


ROOT = ci_contract.ROOT


class StructureTests(unittest.TestCase):
    def test_paths_and_names_are_unique_and_present(self):
        paths = [workflow.path for workflow in ci_contract.WORKFLOWS]
        names = [workflow.name for workflow in ci_contract.WORKFLOWS]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertEqual(len(names), len(set(names)))
        for workflow in ci_contract.WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                self.assertTrue(workflow.path.startswith(ci_contract.WORKFLOW_DIR + "/"))
                self.assertTrue((ROOT / workflow.path).is_file())

    def test_a_name_places_a_workflow_under_its_owner(self):
        owners = set(ci_contract.COMPONENTS) | set(ci_contract.COMPOSITIONS)
        for workflow in ci_contract.WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                parts = workflow.name.split(" / ")
                self.assertIn(workflow.category, ci_contract.CATEGORIES)
                self.assertIn(len(parts), (2, 3))
                self.assertIn(workflow.owner, owners)
                self.assertEqual(parts[1], workflow.owner)

    def test_retired_categories_are_gone(self):
        for workflow in ci_contract.WORKFLOWS:
            for retired in ci_contract.RETIRED_CATEGORIES:
                with self.subTest(workflow=workflow.name, retired=retired):
                    self.assertNotIn(f"{retired} /", workflow.name)
                    self.assertNotEqual(workflow.category, retired)

    def test_display_names_are_title_case_with_labelled_variants(self):
        for workflow in ci_contract.WORKFLOWS:
            for text in (workflow.name, *(job.name for job in workflow.jobs)):
                with self.subTest(text=text):
                    self.assertEqual(ci_contract.title_case_violations(text), [])
                    if "<" in text and not text.startswith("<"):
                        self.assertIn(ci_contract.VARIANT_SEPARATOR, text)

    def test_job_names_are_unique_inside_a_workflow(self):
        for workflow in ci_contract.WORKFLOWS:
            names = [job.name for job in workflow.jobs]
            with self.subTest(workflow=workflow.name):
                self.assertEqual(len(names), len(set(names)))
                self.assertTrue(names)


class RoutingTests(unittest.TestCase):
    def test_every_job_declares_a_class_and_a_budget(self):
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                with self.subTest(job=f"{workflow.name} / {job.name}"):
                    self.assertIn(job.trigger, ci_contract.TRIGGER_CLASSES)
                    self.assertGreater(job.timeout_minutes, 0)

    def test_pull_request_work_stays_inside_the_bound(self):
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                if job.trigger != "pr":
                    continue
                with self.subTest(job=f"{workflow.name} / {job.name}"):
                    self.assertLessEqual(job.timeout_minutes, ci_contract.PR_BOUNDED_MINUTES)

    def test_a_bounded_workflow_carries_full_work_only_by_exception(self):
        for workflow in ci_contract.WORKFLOWS:
            if "pull_request" not in workflow.triggers:
                continue
            for job in workflow.jobs:
                if job.trigger == "full":
                    with self.subTest(job=f"{workflow.name} / {job.name}"):
                        self.assertTrue(job.exception)

    def test_a_pull_request_workflow_declares_the_push_trigger(self):
        for workflow in ci_contract.WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                if "pull_request" in workflow.triggers:
                    self.assertIn("push", workflow.triggers)

    def test_scope_kinds_name_the_workflow_that_uses_them(self):
        used = {}
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                if job.scope:
                    used.setdefault(job.scope, set()).add(workflow.name)
        self.assertEqual(set(used), set(ci_contract.SCOPE_KINDS))
        for kind, workflows in used.items():
            with self.subTest(kind=kind):
                self.assertIn(ci_contract.SCOPE_KINDS[kind], workflows)
        self.assertTrue(set(SCENARIOS) <= set(ci_contract.SCOPE_KINDS))


class OwnershipTests(unittest.TestCase):
    def test_every_consonance_crate_has_one_owning_job(self):
        self.assertEqual(ci_contract.unowned_consonance_crates(), ())
        seen = set()
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                for crate in job.crates:
                    with self.subTest(crate=crate):
                        self.assertNotIn(crate, seen)
                    seen.add(crate)

    def test_every_vmm_core_integration_test_has_an_owner(self):
        self.assertEqual(ci_contract.unowned_vmm_core_tests(), ())
        self.assertTrue(ci_contract.vmm_core_test_targets())

    def test_partitions_split_a_package_without_overlap(self):
        for package, partition in ci_contract.LIB_PARTITIONS.items():
            prefixes = [prefix for group in partition.values() for prefix in group]
            with self.subTest(package=package):
                self.assertEqual(len(prefixes), len(set(prefixes)))
            jobs = {job.name for workflow in ci_contract.WORKFLOWS for job in workflow.jobs}
            for name in partition:
                with self.subTest(package=package, job=name):
                    self.assertIn(name, jobs)

    def test_a_partition_filter_selects_only_its_own_modules(self):
        expression = ci_contract.nextest_filter("searcher", "Archive")
        self.assertIn("test(/^search::archive::/)", expression)
        self.assertNotIn("continuation", expression)
        orphans = ci_contract.nextest_orphan_filter("searcher")
        self.assertTrue(orphans.startswith("not ("))
        self.assertIn("search::campaign", orphans)

    def test_every_miri_target_has_an_owning_component(self):
        self.assertEqual({target["name"] for target in miri_scope.TARGETS},
                         set(ci_contract.MIRI_OWNERS))
        for name, owner in ci_contract.MIRI_OWNERS.items():
            with self.subTest(target=name):
                self.assertIn(owner, ci_contract.MIRI_ANALYSIS_WORKFLOWS)
        for owner, name in ci_contract.MIRI_ANALYSIS_WORKFLOWS.items():
            with self.subTest(owner=owner):
                self.assertEqual(ci_contract.by_name(name).owner, owner)

    def test_registered_manifests_and_dependency_checks_exist(self):
        for manifest in ci_contract.CARGO_MANIFESTS:
            with self.subTest(manifest=manifest):
                self.assertTrue((ROOT / manifest).is_file())
        for command in ci_contract.DENY_COMMANDS:
            manifest = command.split()[1]
            with self.subTest(command=command):
                self.assertIn(manifest, ci_contract.CARGO_MANIFESTS + (
                    "consonance/control-proto/fuzz/Cargo.toml",))
                self.assertTrue((ROOT / manifest).is_file())


class CompositionTests(unittest.TestCase):
    def test_both_nes_compositions_stay_registered(self):
        self.assertEqual(set(ci_contract.NES_COMPOSITIONS),
                         {"Dissonance Workloads", "Harmony Workloads"})
        backends = set()
        for entry in ci_contract.NES_COMPOSITIONS.values():
            backends.add(entry["backend"])
            for role in ("checks", "benchmarks"):
                workflow = ci_contract.by_name(entry[role])
                with self.subTest(workflow=workflow.name):
                    self.assertTrue(workflow.name.endswith("NES"))
                    self.assertEqual(workflow.category,
                                     "Checks" if role == "checks" else "Benchmarks")
        self.assertEqual(backends, {"native", "consonance"})

    def test_required_media_names_a_registered_capture(self):
        registered = ci_contract.CAPTURE_ACTIONS + ci_contract.CAPTURE_SCRIPTS
        self.assertTrue(ci_contract.MEDIA_REQUIRED)
        for (workflow, job), media in ci_contract.MEDIA_REQUIRED.items():
            for capture in media:
                with self.subTest(job=f"{workflow} / {job}"):
                    self.assertIn(capture, registered)
                    self.assertTrue((ROOT / capture).exists())

    def test_historical_scenarios_are_named_after_their_bug(self):
        names = ci_contract.historical_display_names()
        self.assertTrue(names)
        for case_id, display in names.items():
            with self.subTest(case=case_id):
                self.assertEqual(ci_contract.title_case_violations(display), [])
                self.assertNotIn("fixed", display.lower())

    def test_a_case_never_declares_a_comparison_arm(self):
        for path in sorted((ROOT / ci_contract.HISTORICAL_CASE_ROOT).glob("*/case.json")):
            case = json.loads(path.read_text())
            for key in ci_contract.FORBIDDEN_HISTORICAL_KEYS:
                with self.subTest(case=path.parent.name, key=key):
                    self.assertNotIn(key, case)


class HostCompatibilityTests(unittest.TestCase):
    def setUp(self):
        import yaml

        self.workflow = ci_contract.by_name("Checks / Harmony Host Compatibility")
        self.data = yaml.safe_load((ROOT / self.workflow.path).read_text())

    def test_both_hosts_are_checked_on_every_pull_request(self):
        self.assertEqual([job.name for job in self.workflow.jobs], ["macOS Arm64", "Linux Arm64"])
        for job in self.workflow.jobs:
            with self.subTest(job=job.name):
                self.assertEqual(job.trigger, "pr")

    def test_each_job_names_an_explicit_runner(self):
        runners = {job["name"]: job["runs-on"] for job in self.data["jobs"].values()}
        self.assertEqual(runners, {"macOS Arm64": "macos-14", "Linux Arm64": "ubuntu-24.04-arm"})
        for name, runner in runners.items():
            with self.subTest(job=name):
                self.assertNotIn("latest", runner)

    def test_a_host_job_runs_more_than_a_compile(self):
        for job in self.data["jobs"].values():
            commands = "\n".join(str(step.get("run", "")) for step in job["steps"])
            with self.subTest(job=job["name"]):
                self.assertIn("cargo nextest run --workspace", commands)
                self.assertIn("scripts/check-portable-tests.sh", commands)
                self.assertIn("preflight --json", commands)

    def test_a_host_job_claims_no_live_hypervisor(self):
        text = (ROOT / self.workflow.path).read_text()
        self.assertIn("not evidence that HVF or Arm KVM executes a guest", text)
        for job in self.data["jobs"].values():
            commands = "\n".join(
                line for step in job["steps"]
                for line in str(step.get("run", "")).splitlines()
                if not line.lstrip().startswith("#"))
            with self.subTest(job=job["name"]):
                self.assertIsNone(re.search(r"test .*hypervisor.*=", commands))
                self.assertNotIn("/dev/kvm", commands)


class IgnoredTestRegistryTests(unittest.TestCase):
    PATTERN = re.compile(r"[a-z][\w-]*(?:::[\w-]+)? (?:\*|\w+(?:::\w+)*)")

    def test_every_pattern_names_a_binary_and_a_test(self):
        for pattern, runners in ci_contract.ignored_test_runners().items():
            with self.subTest(pattern=pattern):
                self.assertRegex(pattern, self.PATTERN)
                self.assertEqual(len(runners), len(set(runners)))

    def test_host_tests_are_keyed_by_a_machine(self):
        for host in ci_contract.HOST_TESTS:
            with self.subTest(host=host):
                self.assertRegex(host, r"^(?:macos|linux)-(?:aarch64|x86_64)$")

    def test_a_manual_test_says_what_it_is_for_and_nothing_else_runs_it(self):
        runners = ci_contract.ignored_test_runners()
        for pattern, purpose in ci_contract.MANUAL_TESTS.items():
            with self.subTest(pattern=pattern):
                self.assertTrue(purpose.strip())
                self.assertEqual(runners[pattern], (f"by hand: {purpose}",))

    def test_a_pattern_selects_its_whole_binary_or_one_exact_test(self):
        with mock.patch.object(ci_contract, "ignored_test_runners", lambda: {
                "vmm-backend::kvm_smoke *": ("A",), "vmm-core live::one": ("B",)}):
            self.assertEqual(ci_contract.runners_of("vmm-backend::kvm_smoke", "any"), ("A",))
            self.assertEqual(ci_contract.runners_of("vmm-core", "live::one"), ("B",))
            self.assertEqual(ci_contract.runners_of("vmm-core", "live::one_more"), ())
            self.assertEqual(ci_contract.runners_of("vmm-core::live", "one"), ())

    def test_the_host_filter_selects_each_machines_tests(self):
        with mock.patch.object(ci_contract, "HOST_TESTS", {
                "macos-aarch64": ("vmm-backend::hvf_smoke *", "vmm-core live::one")}):
            self.assertEqual(ci_contract.host_filter("macos-aarch64"),
                             "binary_id(vmm-backend::hvf_smoke) | "
                             "(binary_id(vmm-core) & test(=live::one))")
            self.assertEqual(ci_contract.host_filter("linux-x86_64"), "")

    def test_this_host_uses_the_host_test_spelling(self):
        for system, machine, expected in (("Darwin", "arm64", "macos-aarch64"),
                                          ("Linux", "aarch64", "linux-aarch64"),
                                          ("Linux", "x86_64", "linux-x86_64")):
            with self.subTest(expected=expected), \
                 mock.patch.object(ci_contract.platform, "system", lambda: system), \
                 mock.patch.object(ci_contract.platform, "machine", lambda: machine):
                self.assertEqual(ci_contract.this_host(), expected)


class NamingTests(unittest.TestCase):
    def test_title_case_accepts_canonical_spellings(self):
        for name in ("Checks / Harmony Workloads / OCI", "PostgreSQL Index Corruption",
                     "etcd Data Inconsistency", "Snapshot and Restore", "macOS Arm64",
                     "Mutation Testing — Shard <N>/16", "Miri — <Crate> (Whole Crate)"):
            with self.subTest(name=name):
                self.assertEqual(ci_contract.title_case_violations(name), [])

    def test_title_case_rejects_lowercase_words_and_misspelled_terms(self):
        for name in ("Guest memory", "guest Memory", "Public Api", "Postgresql Corruption",
                     "Checks / consonance"):
            with self.subTest(name=name):
                self.assertTrue(ci_contract.title_case_violations(name))

    def test_a_variant_placeholder_is_recognized(self):
        self.assertTrue(ci_contract.is_variant_placeholder("Nova — Level <N>"))
        self.assertFalse(ci_contract.is_variant_placeholder("Nova — Full Game"))


class CommandTests(unittest.TestCase):
    def test_the_command_line_prints_what_workflows_consume(self):
        import contextlib
        import io

        for argv, expected in ((["manifests"], "Cargo.toml"),
                               (["deny"], "--manifest-path"),
                               (["workflows"], "Checks / Consonance"),
                               (["package-flags", "Dissonance"], "-p searcher"),
                               (["nextest-filter", "vmm-core", "Virtual Time"], "virtual_time"),
                               (["nextest-orphans", "vmm-core"], "not ("),
                               (["host-filter", "macos-aarch64"], "binary_id(vmm-backend::hvf_smoke)")):
            with self.subTest(argv=argv):
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.assertEqual(ci_contract._main(argv), 0)
                self.assertIn(expected, output.getvalue())

    def test_an_unknown_command_fails(self):
        with self.assertRaises(SystemExit):
            ci_contract._main(["unknown"])


if __name__ == "__main__":
    unittest.main()
