#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Focused tests for the workflow timeout lint."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("custom-lints.py")
SPEC = importlib.util.spec_from_file_location("custom_lints", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
LINTS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LINTS
SPEC.loader.exec_module(LINTS)


def workflow(job: str, body: str) -> str:
    return f"""name: Checks / timeout test
on:
  pull_request:
jobs:
  {job}:
    runs-on: ubuntu-latest
{body}
"""


class WorkflowTimeoutLintTests(unittest.TestCase):
    def check(self, content: str) -> list[LINTS.Violation]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workflow_path = root / ".github/workflows/test.yml"
            workflow_path.parent.mkdir(parents=True)
            workflow_path.write_text(content)
            return LINTS.check_workflow_rules(root, [".github/workflows/test.yml"])

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

    def test_non_pr_guard_may_have_a_long_timeout(self) -> None:
        content = workflow(
            "deep",
            "    if: github.event_name == 'schedule'\n    timeout-minutes: 90\n    steps: []",
        )
        self.assertFalse(self.check(content))
        self.assertFalse(
            self.check(workflow("manual_only", "    if: github.event_name == 'workflow_dispatch'\n    steps: []"))
        )

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
        self.assertFalse(self.check(content))

    def test_adjacent_named_exception_passes(self) -> None:
        content = workflow(
            "mutants",
            """    # ci-pr-job-timeout-exception: mutants -- sharded mutation coverage is required
    timeout-minutes: 90
    if: github.event_name == 'pull_request'
    steps: []""",
        )
        self.assertFalse(self.check(content))

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
        content = f"""name: Checks / timeout test
on:
  pull_request:
jobs:
  other:
    runs-on: ubuntu-latest
    # ci-pr-job-timeout-exception: mutants -- wrong job name
    timeout-minutes: 90
    steps: []
  mutants:
    runs-on: ubuntu-latest
    timeout-minutes: 90
    steps: []
"""
        violations = self.check(content)
        self.assertEqual([v.rule for v in violations], ["ci-pr-job-timeout", "ci-pr-job-timeout"])


if __name__ == "__main__":
    unittest.main()
