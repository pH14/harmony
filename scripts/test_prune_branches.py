#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Network-free tests for prune-branches.py."""

from __future__ import annotations

import importlib.util
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


def pr(number, state, head=TIP, base="main", cross=False):
    return {"number": number, "state": state, "head_oid": head,
            "base": base, "cross_repository": cross}


def branch(name="feature", oid=TIP, prs=()):
    return {"name": name, "oid": oid, "prs": list(prs)}


def classify(candidate, on_default=False):
    return PRUNE.classify(candidate, "main", lambda oid: on_default)


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
        verdict, _ = classify(branch(prs=[pr(9, "MERGED", cross=True)]))
        self.assertEqual(verdict, PRUNE.KEEP)

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


class PathTests(unittest.TestCase):
    def test_slashes_stay_and_other_characters_are_quoted(self):
        self.assertEqual(
            PRUNE.delete_path("o/r", "claude/fix #1"),
            "repos/o/r/git/refs/heads/claude/fix%20%231")


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
