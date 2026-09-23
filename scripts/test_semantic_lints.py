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
import contextlib
import io
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

ROOT = SCRIPT.parent.parent
sys.path.insert(0, str(SCRIPT.parent))
import ci_contract


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
    one_off_program=0.05,
    workload="none",
    workload_confidence=0.9,
    **architecture: float,
) -> dict:
    answers = {
        "file_kind": choice(file_kind, file_kind_confidence),
        "records_runs": noul(records_runs),
        "status_narrative": noul(status_narrative),
        "decision_residue": noul(decision_residue),
        "one_off_program": noul(one_off_program),
        "workload_named": choice(workload, workload_confidence),
    }
    for question_id in (*LINTS.CI_ARCHITECTURE_RULES, *LINTS.GUEST_CONTRACT_RULES):
        answers[question_id] = noul(architecture.pop(question_id, 0.05))
    assert not architecture, sorted(architecture)
    return answers


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

    def test_request_sends_a_non_default_user_agent(self):
        seen = []

        def post(url: str, headers: dict, body: bytes) -> bytes:
            seen.append(headers)
            return make_post(full_answers())(url, headers, body)

        LINTS.ask({}, {"records_runs": LINTS.QUESTIONS["records_runs"]}, post=post)
        user_agent = seen[0].get("User-Agent", "")
        self.assertTrue(user_agent)
        self.assertFalse(user_agent.startswith("Python-urllib"))

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

    def test_a_program_whose_caller_was_removed_is_selected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._git(root, "init", "-q")
            self._git(root, "config", "user.email", "test@example.com")
            self._git(root, "config", "user.name", "Test")

            (root / "tool" / "src" / "bin").mkdir(parents=True)
            (root / "tool" / "src" / "bin" / "probe.rs").write_text("fn main() {}\n")
            (root / "tool" / "src" / "bin" / "probe_two.rs").write_text("fn main() {}\n")
            (root / "scripts").mkdir()
            (root / "scripts" / "sweep.py").write_text("print()\n")
            (root / "scripts" / "helpers.py").write_text("X = 1\n")
            (root / "scripts" / "report.py").write_text("import helpers\n")
            (root / "run.sh").write_text(
                "cargo run --bin probe\ncargo run --bin probe_two\npython3 scripts/sweep.py\n")
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "base")

            (root / "run.sh").write_text("cargo run --bin probe_two\n")
            (root / "scripts" / "report.py").write_text("X = 2\n")
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "drop callers")

            self.assertEqual(
                sorted(LINTS.programs_losing_a_caller(root, "HEAD~1")),
                ["scripts/helpers.py", "scripts/sweep.py", "tool/src/bin/probe.rs"],
            )

    def test_a_removed_caller_is_matched_within_one_line(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._git(root, "init", "-q")
            self._git(root, "config", "user.email", "test@example.com")
            self._git(root, "config", "user.name", "Test")

            (root / "scripts").mkdir()
            (root / "scripts" / "helpers.py").write_text("X = 1\n")
            (root / "scripts" / "sweep.py").write_text("print()\n")
            (root / "notes.txt").write_text("we import\nhelpers later\n")
            (root / "query.sql").write_text("-- python3 scripts/sweep.py\nselect 1;\n")
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "base")

            (root / "notes.txt").write_text("nothing\n")
            (root / "query.sql").write_text("select 1;\n")
            self._git(root, "add", "-A")
            self._git(root, "commit", "-q", "-m", "edit")

            self.assertEqual(
                LINTS.programs_losing_a_caller(root, "HEAD~1"),
                ["scripts/sweep.py"],
            )

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


class CiArchitectureTests(RequiresApiKey):
    """The CI architecture questions: scope, context, invalidation and verdicts."""

    def plant(self, root: Path, path: str, content: str) -> None:
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(content)

    def workflow_path(self) -> str:
        return ci_contract.WORKFLOWS[0].path

    def test_workflow_questions_are_asked_only_of_workflows(self):
        asked = set(LINTS.questions_for(self.workflow_path()))
        self.assertTrue(set(LINTS.WORKFLOW_QUESTION_IDS) <= asked)
        self.assertFalse(set(LINTS.CI_DOCUMENTATION_QUESTION_IDS) <= asked)
        for path in (".github/actions/nes-film/action.yml", "workloads/nes/src/film.rs",
                     "benchmarks/search/eval.py"):
            with self.subTest(path=path):
                self.assertFalse(set(LINTS.WORKFLOW_QUESTION_IDS) <= set(LINTS.questions_for(path)))

    def test_the_seed_question_follows_the_workflows_and_their_scripts(self):
        for path in (self.workflow_path(), "scripts/historical-search.sh"):
            with self.subTest(path=path):
                self.assertIn("seed_outcome_pinned", LINTS.questions_for(path))
        for path in ("docs/WORKFLOWS.md", "workloads/nes/src/film.rs",
                     "scripts/historical-manifest.py"):
            with self.subTest(path=path):
                self.assertNotIn("seed_outcome_pinned", LINTS.questions_for(path))

    def test_a_workflow_requiring_one_seed_to_find_the_bug_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.workflow_path()
            self.plant(root, path, "name: Checks / Repository\non:\n  pull_request:\n")
            failures, _, _, _, _, _ = LINTS.run(
                root, [path], {},
                post=make_post(full_answers(file_kind="config", seed_outcome_pinned=0.97)))
            self.assertEqual([rule for rule, _, _ in failures], ["ci-pinned-seed-outcome"])

    def test_documentation_questions_follow_the_documentation_scope(self):
        for path in ("docs/WORKFLOWS.md", "workloads/bugs/historical/README.md",
                     "consonance/vmm-core/README.md", "CONTRIBUTING.md"):
            with self.subTest(path=path):
                self.assertTrue(set(LINTS.CI_DOCUMENTATION_QUESTION_IDS)
                                <= set(LINTS.questions_for(path)))
        for path in ("dissonance/searcher/src/lib.rs", "benchmarks/search/nightly.json"):
            with self.subTest(path=path):
                self.assertFalse(set(LINTS.CI_DOCUMENTATION_QUESTION_IDS)
                                 & set(LINTS.questions_for(path)))

    def test_a_workflow_is_judged_with_its_actions_scripts_and_contract(self):
        path = ci_contract.by_name("Checks / Dissonance Workloads / NES").path
        content = (ROOT / path).read_text()
        context = LINTS.context_for(ROOT, path, content)
        self.assertEqual(context["registered"]["name"], "Checks / Dissonance Workloads / NES")
        self.assertIn(".github/actions/ci-scope/action.yml", context["referenced"])
        self.assertIn("scripts/verify-nes-films.py", context["referenced"])
        self.assertIn("docs/WORKFLOWS.md", context["documentation"])
        self.assertIn("Harmony Workloads", context["policy"]["compositions"])
        self.assertTrue(any(entry["name"] == "Checks / Harmony Workloads / NES"
                            for entry in context["other_workflows"]))

    def test_the_composed_context_stays_bounded(self):
        for workflow in ci_contract.WORKFLOWS:
            content = (ROOT / workflow.path).read_text()
            context = LINTS.context_for(ROOT, workflow.path, content)
            excerpts = [entry["text"] for entry in context["referenced"].values()
                        if "text" in entry]
            with self.subTest(workflow=workflow.name):
                self.assertLessEqual(len(excerpts), LINTS.CONTEXT_FILE_COUNT)
                self.assertLessEqual(sum(len(body) for body in excerpts),
                                     LINTS.CONTEXT_TOTAL_LIMIT + LINTS.CONTEXT_FILE_LIMIT)
                self.assertTrue(all(len(entry["sha256"]) == 64
                                    for entry in context["referenced"].values()))

    def test_a_script_reached_through_an_action_is_a_dependency(self):
        path = ci_contract.by_name("Benchmarks / Dissonance Workloads / NES").path
        content = (ROOT / path).read_text()
        self.assertNotIn("scripts/verify-nes-films.py", content)
        self.assertIn("scripts/verify-nes-films.py", LINTS.referenced_paths(ROOT, content))
        self.assertIn(path, LINTS.select_files(ROOT, ["scripts/verify-nes-films.py"]))

    def test_a_registered_media_renderer_is_a_dependency(self):
        path = ci_contract.by_name("Benchmarks / Harmony Workloads / NES").path
        content = (ROOT / path).read_text()
        self.assertIn("workloads/nes/src/bin/nes-film.rs",
                      LINTS._dependencies(ROOT, path, content))
        self.assertIn(path, LINTS.select_files(ROOT, ["workloads/nes/src/bin/nes-film.rs"]))

    def test_a_change_past_the_context_excerpt_invalidates_the_judgment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.workflow_path()
            content = ("name: Checks / Repository\non:\n  pull_request:\n"
                       "jobs:\n  one:\n    steps:\n"
                       "      - run: python3 scripts/verify-nes-films.py\n")
            self.plant(root, path, content)
            self.plant(root, "docs/WORKFLOWS.md", "The registry owns every workflow.\n")
            padding = "# " + "x" * (LINTS.CONTEXT_FILE_LIMIT + 32) + "\n"
            self.plant(root, "scripts/verify-nes-films.py", padding + "MARKER = 1\n")
            first = LINTS._cache_key(path, content, LINTS.questions_for(path),
                                     LINTS.context_for(root, path, content))
            self.plant(root, "scripts/verify-nes-films.py", padding + "MARKER = 2\n")
            second = LINTS._cache_key(path, content, LINTS.questions_for(path),
                                      LINTS.context_for(root, path, content))
            self.assertNotEqual(first, second)

    def test_a_changed_dependency_reselects_and_rejudges_the_workflow(self):
        path = ci_contract.by_name("Checks / Dissonance Workloads / NES").path
        selected = LINTS.select_files(ROOT, ["scripts/verify-nes-films.py"])
        self.assertIn(path, selected)
        self.assertIn(path, LINTS.select_files(ROOT, ["scripts/ci_contract.py"]))
        self.assertIn(path, LINTS.select_files(ROOT, ["docs/WORKFLOWS.md"]))
        self.assertNotIn(path, LINTS.select_files(ROOT, ["README.md"]))

    def test_a_dependency_change_invalidates_the_cached_judgment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.workflow_path()
            self.plant(root, path, "name: Checks / Repository\non:\n  pull_request:\n")
            self.plant(root, "docs/WORKFLOWS.md", "The registry owns every workflow.\n")
            calls: list = []
            post = make_post(full_answers(file_kind="config"), calls)
            cache: dict = {}
            LINTS.run(root, [path], {}, post=post, cache=cache)
            LINTS.run(root, [path], {}, post=post, cache=cache)
            self.assertEqual(len(calls), 1)
            self.plant(root, "docs/WORKFLOWS.md", "The registry owns every workflow and job.\n")
            LINTS.run(root, [path], {}, post=post, cache=cache)
            self.assertEqual(len(calls), 2)

    def test_the_policy_version_is_part_of_the_cache_key(self):
        questions = LINTS.questions_for("docs/WORKFLOWS.md")
        context = LINTS.context_for(ROOT, "docs/WORKFLOWS.md", "text")
        first = LINTS._cache_key("docs/WORKFLOWS.md", "text", questions, context)
        bumped = json.loads(json.dumps(context))
        bumped["policy"]["version"] += 1
        self.assertNotEqual(first, LINTS._cache_key("docs/WORKFLOWS.md", "text", questions, bumped))

    def test_each_architecture_question_fails_warns_and_passes_by_threshold(self):
        for question_id, rule in LINTS.CI_ARCHITECTURE_RULES.items():
            with self.subTest(question=question_id):
                failed, warned = LINTS.evaluate(full_answers(**{question_id: 0.97}))
                self.assertEqual(failed, [rule])
                failed, warned = LINTS.evaluate(full_answers(**{question_id: 0.90}))
                self.assertEqual((failed, warned), ([], [rule]))
                self.assertEqual(LINTS.evaluate(full_answers(**{question_id: 0.10})), ([], []))

    def test_a_clean_workflow_and_a_clean_document_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.workflow_path()
            self.plant(root, path, "name: Checks / Repository\non:\n  pull_request:\n")
            self.plant(root, "workloads/bugs/historical/README.md",
                       "Each case records the upstream versions the bug affects.\n")
            failures, warnings, _, _, errors, _ = LINTS.run(
                root, [path, "workloads/bugs/historical/README.md"], {},
                post=make_post(full_answers(file_kind="config")))
            self.assertEqual((failures, warnings, errors), ([], [], []))

    def test_documentation_directing_a_fixed_version_campaign_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, "workloads/bugs/historical/README.md",
                       "Run the search against the fixed version as a control arm.\n")
            failures, _, _, _, _, _ = LINTS.run(
                root, ["workloads/bugs/historical/README.md"], {},
                post=make_post(full_answers(file_kind="instructions",
                                            fixed_version_direction=0.96)))
            self.assertEqual([rule for rule, _, _ in failures], ["ci-fixed-version-direction"])

    def test_an_architecture_finding_cannot_be_baselined(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.workflow_path()
            self.plant(root, path, "name: Checks / Repository\non:\n  pull_request:\n")
            answers = full_answers(file_kind="config", disguised_search=0.99)
            baseline = {"ci-disguised-search": [path]}
            failures, _, baselined, _, _, _ = LINTS.run(
                root, [path], baseline, post=make_post(answers))
            self.assertEqual([rule for rule, _, _ in failures], ["ci-disguised-search"])
            self.assertEqual(baselined, set())

    def test_update_baseline_refuses_an_architecture_finding(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.workflow_path()
            self.plant(root, path, "name: Checks / Repository\non:\n  pull_request:\n")
            answers = full_answers(file_kind="config", owner_match=0.99)
            with mock.patch.object(LINTS, "_http_post", make_post(answers)), \
                 mock.patch.object(LINTS, "all_tracked_files", return_value=[path]), \
                 mock.patch.object(LINTS, "save_baseline") as save, \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(LINTS.main(["--repo-root", directory, "--all", "--update-baseline"]), 1)
            save.assert_not_called()

    def test_text_addressed_to_the_judge_is_material_not_instruction(self):
        for question_id in LINTS.CI_ARCHITECTURE_RULES:
            with self.subTest(question=question_id):
                self.assertTrue(
                    LINTS.QUESTIONS[question_id]["instructions"].startswith(LINTS.CONTENT_IS_DATA))

    def test_no_credentials_still_skips_with_the_new_questions(self):
        calls: list = []
        with mock.patch.object(LINTS, "_http_post", make_post(full_answers(), calls)), \
             mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("TYPESAFE_API_KEY", None)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(LINTS.main(["--all"]), 0)
        self.assertEqual(calls, [])


class GuestContractTests(RequiresApiKey):
    def plant(self, root: Path, path: str, content: str) -> None:
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(content)

    def test_question_covers_guest_patches_and_launchers(self):
        included = (
            "consonance/harmony-linux/linux/patches/x86/0010-clock.patch",
            "consonance/harmony-linux/linux/patches/arm64/0013-clock.patch",
            "consonance/harmony-linux/linux/build-kernel.sh",
            "consonance/harmony-linux/linux/x86-n6-traps-off-config-fragment",
            "consonance/client/src/session.rs",
            "cli/src/oci/runner.rs",
            "workloads/guest-images/verify-prepared-admission.py",
        )
        for path in included:
            with self.subTest(path=path):
                self.assertIn("guest_runtime_opt_in", LINTS.questions_for(path))
        for path in ("dissonance/searcher/src/lib.rs", "README.md"):
            self.assertNotIn("guest_runtime_opt_in", LINTS.questions_for(path))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = included[0]
            self.plant(root, path, "--- a/arch/x86/kernel/tsc.c\n")
            self.assertIn(path, LINTS.select_files(root, [path]))

    def test_earlier_kernel_patch_receives_final_series_patch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = "consonance/harmony-linux/linux/patches/x86/0001-clock.patch"
            tail = "consonance/harmony-linux/linux/patches/x86/0009-required.patch"
            self.plant(root, old, "Make clock optional at this intermediate step.\n")
            self.plant(root, tail, "Require clock registration at every boot.\n")
            context = LINTS.context_for(root, old, (root / old).read_text())
            self.assertEqual(context["final_series_patch"][tail]["text"],
                             "Require clock registration at every boot.\n")

    def test_runtime_opt_in_fails_and_cannot_be_baselined(self):
        path = "consonance/harmony-linux/linux/patches/x86/0010-clock.patch"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, path,
                       "Boot with harmony_clock=1; otherwise use the host TSC "
                       "so this same guest works on ordinary KVM.\n")
            findings, _, baselined, _, errors, _ = LINTS.run(
                root, [path], {"guest-runtime-opt-in": [path]},
                post=make_post(full_answers(guest_runtime_opt_in=0.98)))
            self.assertEqual([rule for rule, _, _ in findings], ["guest-runtime-opt-in"])
            self.assertEqual((baselined, errors), (set(), []))

    def test_build_only_negative_control_passes(self):
        path = "consonance/harmony-linux/linux/x86-n6-traps-off-config-fragment"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.plant(root, path,
                       "Disable user counter traps in this separately built N6 "
                       "instruction test image; keep the Harmony clock required.\n")
            self.assertIn(path, LINTS.select_files(root, [path]))
            self.assertIn("guest_runtime_opt_in", LINTS.questions_for(path))
            findings, warnings, _, _, errors, _ = LINTS.run(
                root, [path], {},
                post=make_post(full_answers(guest_runtime_opt_in=0.03)))
            self.assertEqual((findings, warnings, errors), ([], [], []))

    def test_question_distinguishes_runtime_fallback_from_build_control(self):
        question = LINTS.QUESTIONS["guest_runtime_opt_in"]
        self.assertTrue(question["instructions"].startswith(LINTS.CONTENT_IS_DATA))
        self.assertIn("build-time", question["instructions"])
        self.assertIn("stop boot", question["instructions"])


class OneOffProgramTests(RequiresApiKey):
    def plant(self, root: Path, path: str, content: str) -> None:
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(content)

    def test_the_question_is_asked_only_of_standalone_programs(self):
        for path in ("consonance/vmm-backend/src/bin/x86_kvm_probe.rs",
                     "scripts/ci_contract.py", "consonance/harmony-linux/linux/build-kernel.sh"):
            with self.subTest(path=path):
                self.assertIn("one_off_program", LINTS.questions_for(path))
        for path in ("scripts/test_ci_contract.py", "harmony-cli/src/main.rs",
                     "workloads/nes/src/bin/smb-probe.rs", "workloads/nes/tools/fm2_to_prefix.py",
                     "consonance/vmm-backend/tests/hvf_smoke.rs",
                     "consonance/vmm-backend/src/hvf.rs", "docs/TESTING.md"):
            with self.subTest(path=path):
                self.assertNotIn("one_off_program", LINTS.questions_for(path))

    def test_the_context_lists_every_other_line_that_names_the_program(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = "tools/src/bin/page_probe.rs"
            self.plant(root, path, "fn main() { let page_probe = 1; }\n")
            self.plant(root, "tools/README.md", "Run `page_probe` to read one page.\n")
            self.plant(root, ".github/workflows/checks.yml",
                       "run: cargo run --bin page-probe\nrun: cargo run --bin page_probe_all\n")
            context = LINTS.context_for(root, path, (root / path).read_text())
            self.assertEqual(context, {
                "reference_count": 2,
                "references": [
                    ".github/workflows/checks.yml:1: run: cargo run --bin page-probe",
                    "tools/README.md:1: Run `page_probe` to read one page.",
                ],
            })

    def test_a_python_module_is_named_by_its_path_or_an_import(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = "scripts/scope.py"
            self.plant(root, path, "print('scope')\n")
            self.plant(root, "scripts/check.py", "from scope import files\nscope = 3\n")
            self.plant(root, "scripts/run.sh", "python3 scripts/scope.py\n")
            context = LINTS.context_for(root, path, (root / path).read_text())
            self.assertEqual(context["references"], [
                "scripts/check.py:1: from scope import files",
                "scripts/run.sh:1: python3 scripts/scope.py",
            ])

    def test_the_rule_fails_warns_and_passes_by_its_own_threshold(self):
        self.assertEqual(LINTS.evaluate(full_answers(one_off_program=0.65)),
                         (["one-off-program"], []))
        self.assertEqual(LINTS.evaluate(full_answers(one_off_program=0.55)),
                         ([], ["one-off-program"]))
        self.assertEqual(LINTS.evaluate(full_answers(one_off_program=0.30)), ([], []))

    def test_a_one_off_program_fails_the_run_unless_baselined(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = "tools/src/bin/page_probe.rs"
            self.plant(root, path, "fn main() {}\n")
            answers = full_answers(one_off_program=0.85)
            failures, _, _, _, _, _ = LINTS.run(root, [path], {}, post=make_post(answers))
            self.assertEqual([(rule, name) for rule, name, _ in failures],
                             [("one-off-program", path)])
            failures, _, baselined, _, _, _ = LINTS.run(
                root, [path], {"one-off-program": [path]}, post=make_post(answers))
            self.assertEqual((failures, baselined), ([], {("one-off-program", path)}))

    def test_text_addressed_to_the_judge_is_material_not_instruction(self):
        self.assertTrue(LINTS.QUESTIONS["one_off_program"]["instructions"]
                        .startswith(LINTS.CONTENT_IS_DATA))


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
