#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Inline selection must preserve test coverage and fail closed on diff errors."""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from ci_scope import SCENARIOS, selected
from miri_scope import TARGETS, selected_targets
from quality_scope import kani_required

SPEC = importlib.util.spec_from_file_location("ci_job_scope", Path(__file__).with_name("ci-job-scope.py"))
SCOPE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SCOPE)


class InlineSelectionTests(unittest.TestCase):
    def test_reads_a_real_git_diff(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "origin"
            root.mkdir()
            def git(*args):
                return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null",
                                                "-c", "commit.gpgsign=false",
                                                "-c", "user.name=CI test", "-c", "user.email=ci@example.invalid",
                                                *args], cwd=root, text=True)
            git("init", "-q")
            git("config", "uploadpack.allowFilter", "true")
            (root / "README.md").write_text("baseline\n")
            source = root / "consonance/vmm-backend/src/kvm.rs"
            source.parent.mkdir(parents=True)
            source.write_text("moved implementation\n")
            (root / "legacy.bin").write_bytes(b"historical content\0" * 8192)
            git("add", ".")
            git("commit", "-qm", "baseline")
            base = git("rev-parse", "HEAD").strip()
            legacy_blob = git("rev-parse", "HEAD:legacy.bin").strip()
            (root / "legacy.bin").unlink()
            destination = root / "dissonance/searcher/src/kvm.rs"
            destination.parent.mkdir(parents=True)
            source.rename(destination)
            target = root / "workloads/nes/src/stb/target.rs"
            target.parent.mkdir(parents=True)
            target.write_text("changed\n")
            git("add", ".")
            git("commit", "-qm", "workload change")
            clone = Path(directory) / "partial"
            subprocess.check_call(["git", "clone", "-q", "--filter=blob:none", root.as_uri(), str(clone)])
            run = subprocess.check_output
            self.assertEqual(run(["git", "rev-parse", "--is-shallow-repository"], cwd=clone, text=True).strip(), "false")
            missing = run(["git", "rev-list", "--objects", "--all", "--missing=print"], cwd=clone, text=True)
            self.assertIn("?" + legacy_blob, missing.splitlines())
            with mock.patch.object(SCOPE.subprocess, "check_output", side_effect=lambda args, **kwargs: run(args, cwd=clone, **kwargs)):
                paths = SCOPE.changed_paths("dissonance_stb", "pull_request", base, "")
            self.assertEqual(paths, ["consonance/vmm-backend/src/kvm.rs", "dissonance/searcher/src/kvm.rs",
                                     "legacy.bin", "workloads/nes/src/stb/target.rs"])
            self.assertTrue(SCOPE.selection("dissonance_stb", "", paths)["enabled"])
            for kind in ("dissonance_nes", "harmony_nes", "consonance_platform", "consonance_kvm"):
                self.assertTrue(SCOPE.selection(kind, "", paths)["enabled"])
            self.assertTrue(SCOPE.selection("public_api", "", paths)["enabled"])
            missing = run(["git", "rev-list", "--objects", "--all", "--missing=print"], cwd=clone, text=True)
            self.assertIn("?" + legacy_blob, missing.splitlines())

    def test_matches_existing_selectors(self):
        samples = [[], ["docs/WORKFLOWS.md"], ["Cargo.lock"],
                   ["workloads/nes/src/stb/target.rs"], ["cli/src/main.rs"],
                   ["consonance/vmm-backend/src/kvm.rs"], ["consonance/client/src/lib.rs"],
                   ["workloads/fault-policy/src/lib.rs"], ["dissonance/searcher/src/lib.rs"],
                   [".github/workflows/harmony-workloads-historical-bugs.yml"],
                   ["scripts/ci-job-scope.py"],
                   ["workloads/nes-guest/src/lib.rs", "consonance/hypercall-doorbell/src/lib.rs"]]
        for paths in samples:
            with self.subTest(paths=paths):
                for name in SCENARIOS:
                    self.assertEqual(SCOPE.selection(name, "", paths), {"enabled": selected(paths)[name]})
                self.assertEqual(SCOPE.selection("kani", "", paths), {"enabled": kani_required(paths)})
                self.assertEqual(SCOPE.selection("public_api", "", paths), {"enabled": selected(paths)["public_api"]})
                old_targets = {target["name"]: target for target in selected_targets(paths)}
                for target in TARGETS:
                    result = SCOPE.selection("miri", target["name"], paths)
                    expected = old_targets.get(target["name"])
                    self.assertEqual(result["enabled"], expected is not None)
                    self.assertEqual(result["command"], expected["command"] if expected else "")
                    self.assertEqual(result["miriflags"], expected["miriflags"] if expected else "")

    def test_large_diffs_are_not_truncated(self):
        paths = [f"docs/file-{index}.md" for index in range(4000)] + ["workloads/nes/src/stb/target.rs"]
        with mock.patch.object(SCOPE.subprocess, "check_output", return_value="\0".join(paths) + "\0"):
            actual = SCOPE.changed_paths("dissonance_stb", "pull_request", "base", "")
        self.assertEqual(actual, paths)
        self.assertTrue(SCOPE.selection("dissonance_stb", "", actual)["enabled"])

    def test_diff_modes_match_previous_routing_jobs(self):
        for kind in ("dissonance_nes", "kani", "public_api", "miri"):
            with mock.patch.object(SCOPE.subprocess, "check_output", return_value="file\0") as run:
                self.assertEqual(SCOPE.changed_paths(kind, "pull_request", "base", ""), ["file"])
                run.assert_called_once_with(["git", "diff", "--no-renames", "--name-only", "-z", "base...HEAD"], text=True)
        for kind in ("dissonance_nes", "kani", "public_api"):
            with mock.patch.object(SCOPE.subprocess, "check_output", return_value="") as run:
                SCOPE.changed_paths(kind, "push", "", "before")
                suffix = ["before...HEAD"] if kind in SCENARIOS else ["before", "HEAD"]
                run.assert_called_once_with(["git", "diff", "--no-renames", "--name-only", "-z", *suffix], text=True)
            for before in ("", "0" * 40):
                with mock.patch.object(SCOPE.subprocess, "check_output", return_value="") as run:
                    SCOPE.changed_paths(kind, "push", "", before)
                    run.assert_called_once_with(["git", "ls-files", "-z"], text=True)

    def test_unknown_inputs_and_diff_failures_fail_closed(self):
        for kind, target in (("dissonance_nes", "typo"), ("miri", "typo"), ("typo", ""), ("kani", "typo")):
            with self.assertRaises(ValueError):
                SCOPE.selection(kind, target, [])
        for kind, event, base in (("dissonance_nes", "pull_request", ""), ("miri", "pull_request", ""),
                                  ("miri", "merge_group", "base"), ("kani", "unknown", "base")):
            with self.assertRaises(ValueError):
                SCOPE.changed_paths(kind, event, base, "")
        with mock.patch.object(SCOPE.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, "git")):
            with self.assertRaises(subprocess.CalledProcessError):
                SCOPE.changed_paths("dissonance_nes", "pull_request", "base", "")

    def test_cli_distinguishes_not_applicable_from_selected(self):
        for paths, enabled in ((["docs/WORKFLOWS.md"], False), (["dissonance/searcher/src/lib.rs"], True)):
            with tempfile.TemporaryDirectory() as directory:
                summary = Path(directory) / "summary"
                stdout = io.StringIO()
                with mock.patch.object(sys, "argv", ["ci-job-scope.py", "--kind", "dissonance_nes"]), \
                     mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}), \
                     mock.patch.object(SCOPE, "changed_paths", return_value=paths), contextlib.redirect_stdout(stdout):
                    SCOPE.main()
                self.assertEqual(stdout.getvalue(), f"enabled={str(enabled).lower()}\n")
                self.assertIn("Selected by changed files" if enabled else "Not applicable", summary.read_text())
                if not enabled:
                    self.assertIn("Test steps were not run", summary.read_text())

    def test_cli_diff_failure_cannot_publish_success_or_not_applicable(self):
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            stdout = io.StringIO()
            with mock.patch.object(sys, "argv", ["ci-job-scope.py", "--kind", "kani"]), \
                 mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}), \
                 mock.patch.object(SCOPE, "changed_paths", side_effect=subprocess.CalledProcessError(1, "git")), \
                 contextlib.redirect_stdout(stdout), self.assertRaises(subprocess.CalledProcessError):
                SCOPE.main()
            self.assertEqual(stdout.getvalue(), "")
            self.assertFalse(summary.exists())


if __name__ == "__main__":
    unittest.main()
