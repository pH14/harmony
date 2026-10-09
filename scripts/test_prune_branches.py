#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Network-free tests for prune-branches.py."""

from __future__ import annotations

import importlib.util
import io
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("prune-branches.py")
SPEC = importlib.util.spec_from_file_location("prune_branches", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
PRUNE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PRUNE
SPEC.loader.exec_module(PRUNE)

TIP = "a" * 40
OTHER = "b" * 40


REPO = "o/r"


def pr(number, state, head=TIP, base="main", head_repo=REPO, base_repo=REPO):
    return {"number": number, "state": state, "head_oid": head,
            "base": base, "head_repo": head_repo, "base_repo": base_repo}


def branch(name="feature", oid=TIP, prs=(), truncated=False):
    return {"name": name, "oid": oid, "prs": list(prs), "truncated": truncated}


def classify(candidate, on_default=False):
    return PRUNE.classify(candidate, REPO, "main", lambda oid: on_default)


class ClassifyTests(unittest.TestCase):
    def test_default_branch_is_kept(self):
        verdict, _ = classify(branch("main"), on_default=True)
        self.assertEqual(verdict, PRUNE.KEEP)

    def test_open_pull_request_keeps_branch_even_when_tip_is_merged(self):
        verdict, reason = classify(
            branch(prs=[pr(1, "MERGED"), pr(2, "OPEN")]), on_default=True)
        self.assertEqual(verdict, PRUNE.KEEP)
        self.assertIn("#2", reason)

    def test_merged_pull_request_with_same_tip_is_deleted(self):
        verdict, reason = classify(branch(prs=[pr(7, "MERGED")]))
        self.assertEqual(verdict, PRUNE.DELETE)
        self.assertIn("#7", reason)

    def test_commits_after_the_merged_head_are_kept(self):
        verdict, reason = classify(branch(oid=OTHER, prs=[pr(7, "MERGED")]))
        self.assertEqual(verdict, PRUNE.KEEP)
        self.assertIn("after pull request #7", reason)

    def test_merge_into_another_base_does_not_count(self):
        verdict, _ = classify(branch(prs=[pr(7, "MERGED", base="release")]))
        self.assertEqual(verdict, PRUNE.KEEP)

    def test_fork_pull_request_with_a_matching_name_is_ignored(self):
        verdict, _ = classify(branch(prs=[pr(9, "MERGED", head_repo="fork/r")]))
        self.assertEqual(verdict, PRUNE.KEEP)

    def test_merge_into_another_repository_does_not_count(self):
        verdict, _ = classify(branch(prs=[pr(9, "MERGED", base_repo="down/r")]))
        self.assertEqual(verdict, PRUNE.KEEP)

    def test_open_pull_request_to_another_repository_keeps_branch(self):
        verdict, reason = classify(
            branch(prs=[pr(4, "OPEN", base_repo="down/r")]), on_default=True)
        self.assertEqual(verdict, PRUNE.KEEP)
        self.assertIn("#4", reason)

    def test_open_pull_request_from_a_deleted_fork_is_ignored(self):
        verdict, _ = classify(
            branch(prs=[pr(4, "OPEN", head_repo=None)]), on_default=True)
        self.assertEqual(verdict, PRUNE.DELETE)

    def test_repository_names_compare_without_regard_to_case(self):
        verdict, _ = PRUNE.classify(
            branch(prs=[pr(4, "OPEN", head_repo="O/R")]), "o/R", "main",
            lambda oid: True)
        self.assertEqual(verdict, PRUNE.KEEP)

    def test_a_truncated_pull_request_list_keeps_the_branch(self):
        verdict, reason = classify(
            branch(prs=[pr(7, "MERGED")], truncated=True), on_default=True)
        self.assertEqual(verdict, PRUNE.KEEP)
        self.assertIn("more than", reason)

    def test_tip_on_default_branch_is_deleted(self):
        verdict, _ = classify(branch(), on_default=True)
        self.assertEqual(verdict, PRUNE.DELETE)

    def test_closed_without_merging_is_kept(self):
        verdict, reason = classify(branch(prs=[pr(3, "CLOSED")]))
        self.assertEqual(verdict, PRUNE.KEEP)
        self.assertIn("closed without merging", reason)

    def test_branch_without_pull_request_is_kept(self):
        verdict, _ = classify(branch())
        self.assertEqual(verdict, PRUNE.KEEP)


class DeleteTests(unittest.TestCase):
    def test_deletion_names_the_judged_tip_and_the_full_ref(self):
        calls = []
        original = PRUNE.gh
        PRUNE.gh = lambda *args: calls.append(args) or ""
        try:
            PRUNE.delete_branch("R_1", "claude/fix #1", TIP)
        finally:
            PRUNE.gh = original
        (args,) = calls
        self.assertIn("ref=refs/heads/claude/fix #1", args)
        self.assertIn(f"before={TIP}", args)
        self.assertIn(f"after={PRUNE.NULL_OID}", args)


class GhTests(unittest.TestCase):
    def test_a_failed_call_reports_the_error_from_gh(self):
        failed = subprocess.CompletedProcess(
            ["gh", "api"], 1, stdout="", stderr="gh: Resource not accessible\n")
        original_run, original_stderr = PRUNE.subprocess.run, sys.stderr
        PRUNE.subprocess.run = lambda *args, **kwargs: failed
        sys.stderr = io.StringIO()
        try:
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                PRUNE.gh("api")
            printed = sys.stderr.getvalue()
        finally:
            PRUNE.subprocess.run, sys.stderr = original_run, original_stderr
        self.assertEqual(printed, "gh: Resource not accessible\n")
        self.assertEqual(raised.exception.stderr, "gh: Resource not accessible\n")


class SummaryTests(unittest.TestCase):
    def test_summary_counts_and_hides_open_pull_request_branches(self):
        text = PRUNE.render_summary(
            [("a", "merged")], [], [("main", "default branch"),
                                    ("b", "open pull request #2"),
                                    ("c", "no pull request")], True)
        self.assertIn("Deleted 1 branches", text)
        self.assertIn("Kept 1 branches", text)
        self.assertNotIn("`b`", text)


if __name__ == "__main__":
    unittest.main()
