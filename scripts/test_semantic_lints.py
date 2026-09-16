#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for the Jev-backed content lint. Every test injects a fake network
call; nothing here reaches the network except test_live_one_question."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).with_name("semantic-lints.py")
SPEC = importlib.util.spec_from_file_location("semantic_lints", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
LINTS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LINTS
SPEC.loader.exec_module(LINTS)


def noul(value: float) -> dict:
    return {"noul": value}


def choice(value: str, confidence: float) -> dict:
    return {"choice": value, "probabilities": {value: confidence}, "confidence": confidence}


def full_answers(
    file_kind="code",
    file_kind_confidence=0.9,
    records_runs=0.05,
    status_narrative=0.05,
    decision_residue=0.05,
    workload="none",
    workload_confidence=0.9,
) -> dict:
    return {
        "file_kind": choice(file_kind, file_kind_confidence),
        "records_runs": noul(records_runs),
        "status_narrative": noul(status_narrative),
        "decision_residue": noul(decision_residue),
        "workload_named": choice(workload, workload_confidence),
    }


def make_post(canned_answers: dict, calls: list | None = None):
    """A fake post() that only answers questions actually requested, like
    the real API does, and counts its own invocations."""

    def post(url: str, headers: dict, body: bytes) -> bytes:
        if calls is not None:
            calls.append(json.loads(body))
        payload = json.loads(body)
        requested = payload["questions"].keys()
        answers = {k: v for k, v in canned_answers.items() if k in requested}
        return json.dumps(
            {"model": LINTS.MODEL, "answers": answers, "usage": {"input_tokens": 7, "output_tokens": 3}}
        ).encode()

    return post


class RequiresApiKey(unittest.TestCase):
    def setUp(self) -> None:
        patcher = mock.patch.dict(os.environ, {"TYPESAFE_API_KEY": "test-key"})
        patcher.start()
        self.addCleanup(patcher.stop)


class QuestionScopeTests(RequiresApiKey):
    def test_workload_named_is_scoped_to_searcher_not_workloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            searcher = root / "dissonance" / "searcher" / "src" / "lib.rs"
            searcher.parent.mkdir(parents=True)
            searcher.write_text("// mentions a specific console\n")
            workload = root / "workloads" / "lib.rs"
            workload.parent.mkdir(parents=True)
            workload.write_text("// mentions a specific console\n")

            answers = full_answers(workload="game", workload_confidence=0.9)
            calls: list = []
            post = make_post(answers, calls)

            new_failures, _, _, _, _, _ = LINTS.run(
                root,
                ["dissonance/searcher/src/lib.rs", "workloads/lib.rs"],
                {},
                post=post,
            )

            self.assertEqual(
                [(rule, path) for rule, path, _ in new_failures],
                [("workload-named", "dissonance/searcher/src/lib.rs")],
            )
            workloads_request = calls[1]
            self.assertNotIn("workload_named", workloads_request["questions"])


class VerdictTests(RequiresApiKey):
    def _run_one(self, name: str, content: str, answers: dict, baseline=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
            return LINTS.run(root, [name], baseline or {}, post=make_post(answers))

    def test_high_confidence_run_record_fails(self) -> None:
        answers = full_answers(file_kind="run_record", file_kind_confidence=0.95)
        new_failures, new_warnings, _, _, _, _ = self._run_one(
            "dissonance/PLANTED-RUN.toml", "seed = 42\nhost = \"box1\"\n", answers,
        )
        self.assertEqual([r for r, _, _ in new_failures], ["run-record"])
        self.assertEqual(new_warnings, [])

    def test_medium_probability_run_record_warns_not_fails(self) -> None:
        answers = full_answers(records_runs=0.90)
        new_failures, new_warnings, _, _, _, _ = self._run_one(
            "dissonance/PLANTED-RUN.toml", "seed = 42\nhost = \"box1\"\n", answers,
        )
        self.assertEqual(new_failures, [])
        self.assertEqual([r for r, _, _ in new_warnings], ["run-record"])

    def test_component_reference_passes_clean(self) -> None:
        answers = full_answers(file_kind="component_reference", file_kind_confidence=0.97)
        new_failures, new_warnings, _, _, _, _ = self._run_one(
            "consonance/vmm-core/README.md",
            "This component owns snapshot restore boundaries.",
            answers,
        )
        self.assertEqual(new_failures, [])
        self.assertEqual(new_warnings, [])

    def test_baseline_reports_baselined_and_stale_entry_fails(self) -> None:
        # GONE.toml is judged in this run (unlike an untouched file in a
        # --changed-from sweep) and comes back clean, so it's a genuine fix:
        # stale, not "not checked".
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "dissonance").mkdir()
            (root / "dissonance" / "PLANTED-RUN.toml").write_text("seed = 42\n")
            (root / "dissonance" / "GONE.toml").write_text("this file is clean now\n")

            run_record_answers = full_answers(file_kind="run_record", file_kind_confidence=0.95)
            clean_answers = full_answers(file_kind="component_reference")

            def post(url: str, headers: dict, body: bytes) -> bytes:
                payload = json.loads(body)
                path = payload["state"]["path"]
                answers = run_record_answers if path == "dissonance/PLANTED-RUN.toml" else clean_answers
                return json.dumps(
                    {
                        "model": LINTS.MODEL,
                        "answers": {k: v for k, v in answers.items() if k in payload["questions"]},
                        "usage": {"input_tokens": 1, "output_tokens": 1},
                    }
                ).encode()

            baseline = {
                "run-record": ["dissonance/PLANTED-RUN.toml", "dissonance/GONE.toml"],
            }
            new_failures, _, still_baselined, _, _, judged = LINTS.run(
                root, ["dissonance/PLANTED-RUN.toml", "dissonance/GONE.toml"], baseline, post=post,
            )
            self.assertEqual(new_failures, [])
            self.assertIn(("run-record", "dissonance/PLANTED-RUN.toml"), still_baselined)

            stale = LINTS.stale_baseline_entries(baseline, still_baselined, judged)
            self.assertEqual(stale, [("run-record", "dissonance/GONE.toml")])

    def test_baseline_entry_outside_changed_set_is_not_reported_stale(self) -> None:
        # dissonance/GONE.toml is a known violation but wasn't judged this run
        # (a --changed-from sweep only judges the files that actually changed);
        # only a file this run actually checked can confirm a fix.
        answers = full_answers(file_kind="run_record", file_kind_confidence=0.95)
        baseline = {
            "run-record": ["dissonance/PLANTED-RUN.toml", "dissonance/GONE.toml"],
        }
        _, _, still_baselined, _, _, judged = self._run_one(
            "dissonance/PLANTED-RUN.toml", "seed = 42\n", answers, baseline=baseline,
        )
        self.assertNotIn("dissonance/GONE.toml", judged)

        stale = LINTS.stale_baseline_entries(baseline, still_baselined, judged)
        self.assertEqual(stale, [])


class TruncationTests(unittest.TestCase):
    def test_long_content_is_cut_to_100000_chars(self) -> None:
        content = "a" * 120_000
        state = LINTS._state_for("big.md", content)
        self.assertEqual(len(state["content"]), 100_000)
        self.assertTrue(state["truncated"])

    def test_short_content_is_not_marked_truncated(self) -> None:
        state = LINTS._state_for("small.md", "short")
        self.assertNotIn("truncated", state)
        self.assertEqual(state["content"], "short")


class RetryTests(unittest.TestCase):
    def test_retries_429_twice_then_succeeds(self) -> None:
        call_count = {"n": 0}

        def flaky_post(url: str, headers: dict, body: bytes) -> bytes:
            call_count["n"] += 1
            if call_count["n"] <= 2:
                raise LINTS.JevHTTPError(429, b"slow down")
            payload = json.loads(body)
            answers = {k: full_answers()[k] for k in payload["questions"]}
            return json.dumps(
                {"model": LINTS.MODEL, "answers": answers, "usage": {"input_tokens": 1, "output_tokens": 1}}
            ).encode()

        with mock.patch.object(LINTS.time, "sleep") as sleep_mock, \
             mock.patch.dict(os.environ, {"TYPESAFE_API_KEY": "test-key"}):  # pragma: allowlist secret
            answers = LINTS.ask({"path": "x", "content": "y"}, {"records_runs": LINTS.QUESTIONS["records_runs"]}, post=flaky_post)

        self.assertEqual(call_count["n"], 3)
        self.assertEqual(sleep_mock.call_count, 2)
        sleep_mock.assert_has_calls([mock.call(1), mock.call(2)])
        self.assertIn("records_runs", answers)

    def test_non_retryable_status_raises_immediately(self) -> None:
        def bad_post(url: str, headers: dict, body: bytes) -> bytes:
            raise LINTS.JevHTTPError(422, b"malformed body")

        with mock.patch.dict(os.environ, {"TYPESAFE_API_KEY": "test-key"}):  # pragma: allowlist secret
            with self.assertRaises(LINTS.JevHTTPError):
                LINTS.ask({"path": "x", "content": "y"}, LINTS.QUESTIONS, post=bad_post)

    def test_non_json_body_raises_instead_of_crashing(self) -> None:
        def garbled_post(url: str, headers: dict, body: bytes) -> bytes:
            return b"not json"

        with mock.patch.dict(os.environ, {"TYPESAFE_API_KEY": "test-key"}):  # pragma: allowlist secret
            with self.assertRaises(LINTS.JevHTTPError):
                LINTS.ask({"path": "x", "content": "y"}, {"records_runs": LINTS.QUESTIONS["records_runs"]}, post=garbled_post)

    def test_answers_missing_a_requested_question_raises(self) -> None:
        def incomplete_post(url: str, headers: dict, body: bytes) -> bytes:
            return json.dumps({"model": LINTS.MODEL, "answers": {}, "usage": {}}).encode()

        with mock.patch.dict(os.environ, {"TYPESAFE_API_KEY": "test-key"}):  # pragma: allowlist secret
            with self.assertRaises(LINTS.JevHTTPError):
                LINTS.ask({"path": "x", "content": "y"}, {"records_runs": LINTS.QUESTIONS["records_runs"]}, post=incomplete_post)


class AnswerValidationTests(RequiresApiKey):
    def test_invalid_answers_are_errors_and_never_cached_or_judged(self):
        invalid = []
        for question in ("file_kind", "records_runs", "status_narrative"):
            for value in ({}, None, [], "clean"):
                invalid.append((question, value))
        for value in (-0.1, 1.1, float("nan"), float("inf"), False, "0.1"):
            invalid.append(("records_runs", {"noul": value}))
            invalid.append(("file_kind", {"choice": "code", "confidence": value}))
        for value in ("unknown", None, []):
            invalid.append(("file_kind", {"choice": value, "confidence": 0.9}))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "notes.txt").write_text("Reference material.")
            for question, value in invalid:
                with self.subTest(question=question, value=value):
                    answers = full_answers()
                    answers[question] = value
                    cache = {}
                    failures, warnings, baselined, _, errors, judged = LINTS.run(
                        root, ["notes.txt"], {}, post=make_post(answers), cache=cache,
                    )
                    self.assertEqual((failures, warnings, baselined, judged), ([], [], set(), set()))
                    self.assertEqual([path for path, _ in errors], ["notes.txt"])
                    self.assertEqual(cache, {})

    def test_normalized_score_boundaries_are_valid(self):
        for value in (0, 1, 0.0, 1.0):
            answers = full_answers(records_runs=value, file_kind_confidence=value)
            self.assertTrue(LINTS._valid_answers(answers, LINTS.QUESTIONS))

    def test_nonobject_response_is_reported_as_judge_error(self):
        with self.assertRaises(LINTS.JevHTTPError):
            LINTS.ask({}, LINTS.QUESTIONS, post=lambda *_: b"[]")


class CacheTests(RequiresApiKey):
    def test_second_run_over_same_file_does_not_call_network_again(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "README.md").write_text("This component does X.")
            answers = full_answers(file_kind="component_reference")
            calls: list = []
            post = make_post(answers, calls)
            cache: dict = {}

            LINTS.run(root, ["README.md"], {}, post=post, cache=cache)
            LINTS.run(root, ["README.md"], {}, post=post, cache=cache)

            self.assertEqual(len(calls), 1)

    def test_invalid_cached_verdict_is_rejudged(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            content = "Reference material."
            (root / "notes.txt").write_text(content)
            questions = LINTS.questions_for("notes.txt")
            key = LINTS._cache_key("notes.txt", content, questions)
            cache = {key: {name: {} for name in questions}}
            calls = []
            result = LINTS.run(root, ["notes.txt"], {}, post=make_post(full_answers(), calls), cache=cache)
            self.assertEqual(len(calls), 1)
            self.assertEqual(result[4], [])
            self.assertTrue(LINTS._valid_answers(cache[key], questions))



class PerFileErrorTests(RequiresApiKey):
    def test_one_bad_file_does_not_abort_the_rest_of_the_sweep(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "huge.md").write_text("x" * 200_000)
            (root / "ok.md").write_text("This component does X.")

            def post(url: str, headers: dict, body: bytes) -> bytes:
                payload = json.loads(body)
                if payload["state"]["path"] == "huge.md":
                    raise LINTS.JevHTTPError(400, b'{"detail":{"error_type":"max_tokens_exceeded"}}')
                answers = full_answers(file_kind="component_reference")
                return json.dumps(
                    {
                        "model": LINTS.MODEL,
                        "answers": {k: v for k, v in answers.items() if k in payload["questions"]},
                        "usage": {"input_tokens": 1, "output_tokens": 1},
                    }
                ).encode()

            new_failures, new_warnings, _, _, errors, judged = LINTS.run(
                root, ["huge.md", "ok.md"], {}, post=post,
            )

            self.assertEqual(new_failures, [])
            self.assertEqual(new_warnings, [])
            self.assertEqual([path for path, _ in errors], ["huge.md"])
            self.assertEqual(judged, {"ok.md"})


class ChangedFilesTests(unittest.TestCase):
    def _git(self, root: Path, *args: str) -> None:
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True, text=True)

    def test_changed_from_selects_added_and_modified_not_deleted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._git(root, "init", "-q")
            self._git(root, "config", "user.email", "test@example.com")
            self._git(root, "config", "user.name", "Test")

            (root / "keep.md").write_text("original")
            (root / "remove.md").write_text("bye")
            self._git(root, "add", "keep.md", "remove.md")
            self._git(root, "commit", "-q", "-m", "base")

            (root / "keep.md").write_text("modified")
            (root / "added.md").write_text("new")
            (root / "remove.md").unlink()
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "change")

            candidates = LINTS.changed_files(root, "HEAD~1")
            selected = LINTS.select_files(root, candidates)

            self.assertEqual(sorted(selected), ["added.md", "keep.md"])

    def test_changed_from_selects_renamed_files_by_new_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._git(root, "init", "-q")
            self._git(root, "config", "user.email", "test@example.com")
            self._git(root, "config", "user.name", "Test")

            (root / "old-name.md").write_text("a" * 200 + "\noriginal content\n")
            self._git(root, "add", "old-name.md")
            self._git(root, "commit", "-q", "-m", "base")

            (root / "old-name.md").rename(root / "new-name.md")
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "rename")

            candidates = LINTS.changed_files(root, "HEAD~1")

            self.assertEqual(candidates, ["new-name.md"])

    def test_changed_paths_preserve_unicode_and_control_characters(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._git(root, "init", "-q")
            self._git(root, "config", "user.email", "test@example.com")
            self._git(root, "config", "user.name", "Test")
            self._git(root, "commit", "--allow-empty", "-q", "-m", "base")
            names = ["résumé.md", "tab\tname.md", "line\nname.md"]
            for name in names:
                (root / name).write_text("Reference material.")
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "files")
            selected = LINTS.select_files(root, LINTS.changed_files(root, "HEAD~1"))
            self.assertEqual(sorted(selected), sorted(names))



class MainChangedFromBaselineTests(RequiresApiKey):
    """A --changed-from run only judges the files that changed. A baseline
    entry for a file outside that set must not be reported as fixed, and
    --update-baseline must not drop it either."""

    def _git(self, root: Path, *args: str) -> None:
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True, text=True)

    def _make_repo(self, root: Path) -> None:
        self._git(root, "init", "-q")
        self._git(root, "config", "user.email", "test@example.com")
        self._git(root, "config", "user.name", "Test")

        (root / "docs").mkdir()
        baseline = {"run-record": ["untouched.md"]}
        (root / "docs" / "semantic-lints-baseline.json").write_text(json.dumps(baseline))
        (root / "untouched.md").write_text("a known violation nobody fixed yet")
        (root / "changed.md").write_text("original")
        self._git(root, "add", "-A")
        self._git(root, "commit", "-q", "-m", "base")

        (root / "changed.md").write_text("modified, still clean content")
        self._git(root, "add", "-A")
        self._git(root, "commit", "-q", "-m", "change")

    def test_unjudged_baseline_entry_does_not_fail_the_run(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._make_repo(root)
            post = make_post(full_answers(file_kind="component_reference"))

            with mock.patch.object(LINTS, "_http_post", post):
                code = LINTS.main(["--repo-root", str(root), "--changed-from", "HEAD^1"])

            self.assertEqual(code, 0)

    def test_update_baseline_preserves_an_unjudged_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._make_repo(root)
            post = make_post(full_answers(file_kind="component_reference"))

            with mock.patch.object(LINTS, "_http_post", post):
                code = LINTS.main([
                    "--repo-root", str(root), "--changed-from", "HEAD^1", "--update-baseline",
                ])

            self.assertEqual(code, 0)
            baseline = json.loads((root / "docs" / "semantic-lints-baseline.json").read_text())
            self.assertEqual(baseline, {"run-record": ["untouched.md"]})


class SkipWithoutKeyTests(unittest.TestCase):
    def test_missing_key_skips_and_never_calls_network(self) -> None:
        calls: list = []
        with mock.patch.object(LINTS, "_http_post", make_post(full_answers(), calls)), \
             mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("TYPESAFE_API_KEY", None)
            code = LINTS.main(["--all"])
        self.assertEqual(code, 0)
        self.assertEqual(calls, [])


@unittest.skipUnless(os.environ.get("TYPESAFE_API_KEY"), "live Jev call needs TYPESAFE_API_KEY")
class LiveJevTests(unittest.TestCase):
    def test_live_one_question(self) -> None:
        state = "\n".join(f"line {i}" for i in range(10))
        answers = LINTS.ask(state, {"records_runs": LINTS.QUESTIONS["records_runs"]})
        value = answers["records_runs"]["noul"]
        self.assertIsInstance(value, float)
        self.assertGreaterEqual(value, 0.0)
        self.assertLessEqual(value, 1.0)


if __name__ == "__main__":
    unittest.main()
