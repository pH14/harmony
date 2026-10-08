#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Rust job selection runs the jobs unless every change provably cannot reach them."""

from pathlib import Path
import subprocess
import unittest

import ci_contract
from ci_scope import selected
from rust_scope import REPOSITORY_ONLY, rust_checks_required

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = sorted(Path(workflow.path).name for workflow in ci_contract.WORKFLOWS
                   if any(job.select == "rust_checks" for job in workflow.jobs))


def tracked() -> list[str]:
    return subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT, text=True).split("\0")[:-1]


class SelectionTests(unittest.TestCase):
    def required(self, paths, workflow="dissonance-checks.yml"):
        return rust_checks_required(paths, workflow)

    def test_documentation_alone_selects_nothing(self):
        for path in ("README.md", "docs/WORKFLOWS.md", "consonance/vmm-core/README.md",
                     "docs/diagram.svg", "LICENSE", ".agents/skills/x/SKILL.md", "benchmarks/search/run.py"):
            with self.subTest(path=path):
                self.assertFalse(self.required([path]))

    def test_repository_tooling_selects_nothing(self):
        for path in REPOSITORY_ONLY:
            with self.subTest(path=path):
                self.assertFalse(self.required([path]))

    def test_another_workflows_file_selects_nothing_and_its_own_file_selects(self):
        self.assertFalse(self.required([".github/workflows/repository-checks.yml"]))
        self.assertTrue(self.required([".github/workflows/dissonance-checks.yml"]))
        self.assertTrue(self.required([".github/workflows/repository-checks.yml"], "repository-checks.yml"))
        self.assertFalse(self.required([".github/workflows/release.yml"]))

    def test_rust_sources_and_build_inputs_select(self):
        for path in ("dissonance/searcher/src/lib.rs", "consonance/vmm-core/src/lib.rs", "cli/src/main.rs",
                     "workloads/nes/src/lib.rs", "Cargo.lock", "rust-toolchain.toml", "clippy.toml",
                     "deny.toml", ".cargo/config.toml", "flake.nix", "scripts/ci_contract.py",
                     "scripts/check-test-partition.py", ".github/actions/ci-scope/action.yml"):
            with self.subTest(path=path):
                self.assertTrue(self.required([path]))

    def test_an_unrecognized_path_selects(self):
        self.assertTrue(self.required(["new-top-level-directory/file.txt"]))

    def test_one_reaching_path_selects_a_change_of_documentation(self):
        self.assertTrue(self.required(["docs/WORKFLOWS.md", "cli/src/main.rs"]))

    def test_an_empty_change_selects_nothing(self):
        self.assertFalse(self.required([]))

    def test_a_path_that_selects_a_scenario_selects_every_workflow(self):
        paths = [".github/workflows/harmony-workloads-historical-bugs.yml",
                 "scripts/render-historical-bugs.py",
                 ".github/workflows/harmony-workloads-nes-checks.yml"]
        for workflow in WORKFLOWS:
            for path in paths:
                with self.subTest(workflow=workflow, path=path):
                    self.assertTrue(rust_checks_required([path], workflow))


class InvariantTests(unittest.TestCase):
    def test_every_selecting_workflow_is_registered(self):
        self.assertEqual(len(WORKFLOWS), 8)
        for name in WORKFLOWS:
            self.assertTrue((ROOT / ".github/workflows" / name).is_file())

    def test_repository_tooling_exists_and_is_named_by_no_other_workflow(self):
        others = [path for path in (ROOT / ".github").rglob("*.yml")
                  if path.name != "repository-checks.yml"]
        for tool in REPOSITORY_ONLY:
            with self.subTest(tool=tool):
                self.assertTrue((ROOT / tool).is_file())
                for other in others:
                    self.assertNotIn(Path(tool).name, other.read_text(), other)

    def test_no_rust_source_or_build_file_is_ever_skipped(self):
        reaching = [path for path in tracked()
                    if path.endswith((".rs", ".toml", ".lock", ".c", ".h", ".S", ".config"))
                    and path not in REPOSITORY_ONLY]
        self.assertGreater(len(reaching), 100)
        for workflow in WORKFLOWS:
            for path in reaching:
                self.assertTrue(rust_checks_required([path], workflow), (workflow, path))

    def test_every_file_a_scenario_selects_selects_every_workflow(self):
        for path in tracked():
            if any(selected([path]).values()):
                for workflow in WORKFLOWS:
                    self.assertTrue(rust_checks_required([path], workflow), (workflow, path))


if __name__ == "__main__":
    unittest.main()
