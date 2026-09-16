#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Content lint for the Harmony repository, backed by TypeSafe's Jev model.

Judges file content rather than file name or a fixed word list: run
records, status narratives, decision residue, and workload names in
workload-agnostic code. Skipped entirely without TYPESAFE_API_KEY, so it
never blocks a fork PR or an offline checkout.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Callable

_CUSTOM_LINTS_PATH = Path(__file__).with_name("custom-lints.py")
_SPEC = importlib.util.spec_from_file_location("custom_lints", _CUSTOM_LINTS_PATH)
assert _SPEC is not None and _SPEC.loader is not None
custom_lints = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = custom_lints
_SPEC.loader.exec_module(custom_lints)


API_URL = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-1.13.0"
RETRY_DELAYS = (1, 2, 4, 8, 16)
STATE_CONTENT_LIMIT = 100_000
REQUEST_TIMEOUT_SECONDS = 30

# Calibrated in the pull request that pins MODEL; see its description for the
# known-bad/known-good table these were chosen against.
FAIL_PROBABILITY = 0.94
FAIL_CONFIDENCE = 0.80
WARN_PROBABILITY = 0.88

SEMANTIC_BASELINE_PATH = Path("docs/semantic-lints-baseline.json")
CACHE_PATH = Path(".semantic-lints-cache.json")

TEXT_EXTENSIONS = custom_lints.LINTABLE_EXTENSIONS | {".tsv", ".csv"}
DECISION_RESIDUE_EXTENSIONS = {".md", ".py", ".sh", ".toml"}

SKIP_PATHS = {
    "scripts/custom-lints.py",
    "scripts/semantic-lints.py",
    str(custom_lints.BASELINE_PATH),
    str(SEMANTIC_BASELINE_PATH),
}

# The three *-no-workload-names rules define what "workload-agnostic" scope
# means; reuse them instead of restating the directory list here.
WORKLOAD_SCOPE_RULE_NAMES = (
    "searcher-no-workload-names",
    "consonance-no-workload-names",
    "guest-linux-no-workload-names",
)


# ---------------------------------------------------------------------------
# Questions
# ---------------------------------------------------------------------------

QUESTIONS = {
    "file_kind": {
        "type": "choice",
        "instructions": "What kind of file is this?",
        "criteria": {
            "component_reference": "describes what a component is, its boundaries and how to use it.",
            "instructions": "tells the reader what to do or how to work.",
            "run_record": "results of specific runs: dates, seeds, hashes, host names, measured numbers, ledgers of rounds or experiments.",
            "status_report": "a narrative of what was done, what is in progress, what is next.",
            "code": "source code.",
            "config": "build, CI, package or manifest files.",
            "license": "license or legal text.",
            "fixture": "test data or golden data.",
        },
    },
    "records_runs": {
        "type": "noul",
        "instructions": "Does this file record the outcomes of specific experiment, benchmark or campaign runs?",
        "criteria": {
            "true": "it holds dates, seeds, hashes, host names, run identifiers or measured numbers from runs that happened.",
            "false": "it describes how something works or what to do, with no record of particular runs.",
        },
    },
    "status_narrative": {
        "type": "noul",
        "instructions": "Does this file read as a progress or status report?",
        "criteria": {
            "true": "it says what was done, what is pending, what comes next, or carries status labels such as done, paused, frozen, active.",
            "false": "it is reference, instructions, code or data.",
        },
    },
    "decision_residue": {
        "type": "noul",
        "instructions": "Does this file describe rejected alternatives, changes made in answer to a reviewer, or an earlier name of something?",
        "criteria": {
            "true": "it says what something used to be called, that a change answers a reviewer's comment, or which alternatives were considered and rejected.",
            "false": "it describes only what the thing is now.",
        },
    },
    "workload_named": {
        "type": "choice",
        "instructions": (
            "Does this file name one of Harmony's own workloads: a specific "
            "video game or console, database, or distributed system that "
            "the project models, fuzzes, or reproduces bugs in under "
            "workloads/? Generic infrastructure, build, packaging, or "
            "virtualization tooling the project uses to run or build "
            "things does not count (an emulator such as QEMU, an init "
            "system such as BusyBox, a package manager such as Nix), and "
            "neither does Harmony's own component or crate name."
        ),
        "criteria": {
            "none": "no workload is named, or only generic infrastructure, tooling, or a component name is named.",
            "game": "a specific video game or game console that is one of the project's own workloads.",
            "database_or_distributed_system": "a specific database, distributed system, orchestrator or key-value store that is one of the project's own workloads.",
        },
    },
}


def _is_text_file(path: str) -> bool:
    _, ext = os.path.splitext(path)
    return ext in TEXT_EXTENSIONS


def _in_decision_residue_scope(path: str) -> bool:
    _, ext = os.path.splitext(path)
    return ext in DECISION_RESIDUE_EXTENSIONS


def _workload_scope_rules() -> list:
    by_name = {rule.name: rule for rule in custom_lints.RULES}
    return [by_name[name] for name in WORKLOAD_SCOPE_RULE_NAMES]


def _in_workload_named_scope(path: str) -> bool:
    return any(rule.applies(path) for rule in _workload_scope_rules())


def questions_for(path: str) -> dict:
    selected = {
        "file_kind": QUESTIONS["file_kind"],
        "records_runs": QUESTIONS["records_runs"],
        "status_narrative": QUESTIONS["status_narrative"],
    }
    if _in_decision_residue_scope(path):
        selected["decision_residue"] = QUESTIONS["decision_residue"]
    if _in_workload_named_scope(path):
        selected["workload_named"] = QUESTIONS["workload_named"]
    return selected


# ---------------------------------------------------------------------------
# Network client
# ---------------------------------------------------------------------------

class JevHTTPError(Exception):
    def __init__(self, status: int, body: bytes):
        super().__init__(f"Jev returned {status}: {body!r}")
        self.status = status
        self.body = body


def _http_post(url: str, headers: dict, body: bytes) -> bytes:
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        raise JevHTTPError(error.code, error.read()) from error


def _valid_score(value) -> bool:
    return type(value) in (int, float) and 0 <= value <= 1 and math.isfinite(value)


def _valid_answers(answers, questions: dict) -> bool:
    if not isinstance(answers, dict) or set(answers) != set(questions):
        return False
    for name, question in questions.items():
        answer = answers[name]
        if not isinstance(answer, dict):
            return False
        if question["type"] == "noul":
            if not _valid_score(answer.get("noul")):
                return False
        elif question["type"] == "choice":
            if (not isinstance(answer.get("choice"), str)
                    or answer["choice"] not in question["criteria"]
                    or not _valid_score(answer.get("confidence"))):
                return False
        else:
            return False
    return True


def ask(
    state,
    questions: dict,
    post: Callable[[str, dict, bytes], bytes] = _http_post,
    usage_totals: dict | None = None,
) -> dict:
    api_key = os.environ["TYPESAFE_API_KEY"]
    body = json.dumps({"state": state, "model": MODEL, "questions": questions}).encode()
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    attempt = 0
    while True:
        try:
            raw = post(API_URL, headers, body)
        except JevHTTPError as error:
            if error.status not in (429, 529) or attempt >= len(RETRY_DELAYS):
                raise
            time.sleep(RETRY_DELAYS[attempt])
            attempt += 1
            continue
        break
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as error:
        raise JevHTTPError(0, raw) from error
    answers = data.get("answers") if isinstance(data, dict) else None
    if not _valid_answers(answers, questions):
        raise JevHTTPError(0, raw)
    if usage_totals is not None:
        usage = data.get("usage", {})
        usage_totals["input_tokens"] = usage_totals.get("input_tokens", 0) + usage.get("input_tokens", 0)
        usage_totals["output_tokens"] = usage_totals.get("output_tokens", 0) + usage.get("output_tokens", 0)
    return answers


# ---------------------------------------------------------------------------
# Per-file judging, with a content-addressed cache
# ---------------------------------------------------------------------------

def _state_for(path: str, content: str) -> dict:
    state: dict = {"path": path}
    if len(content) > STATE_CONTENT_LIMIT:
        state["content"] = content[:STATE_CONTENT_LIMIT]
        state["truncated"] = True
    else:
        state["content"] = content
    return state


def _cache_key(path: str, content: str, questions: dict) -> str:
    question_text = json.dumps(questions, sort_keys=True)
    digest = hashlib.sha256()
    digest.update(f"{path}\0{content}\0{MODEL}\0{STATE_CONTENT_LIMIT}\0{question_text}".encode())
    return digest.hexdigest()


def judge_file(
    repo_root: Path,
    path: str,
    cache: dict,
    post: Callable[[str, dict, bytes], bytes] = _http_post,
    usage_totals: dict | None = None,
) -> dict:
    content = (repo_root / path).read_text(errors="replace")
    questions = questions_for(path)
    key = _cache_key(path, content, questions)
    if key in cache and _valid_answers(cache[key], questions):
        return cache[key]
    cache.pop(key, None)
    answers = ask(_state_for(path, content), questions, post=post, usage_totals=usage_totals)
    cache[key] = answers
    return answers


def load_cache(repo_root: Path) -> dict:
    path = repo_root / CACHE_PATH
    if not path.exists():
        return {}
    try:
        with path.open() as f:
            data = json.load(f)
            return data if isinstance(data, dict) else {}
    except (OSError, json.JSONDecodeError):
        return {}


def save_cache(repo_root: Path, cache: dict) -> None:
    path = repo_root / CACHE_PATH
    with path.open("w") as f:
        json.dump(cache, f, indent=2, sort_keys=True)
        f.write("\n")


# ---------------------------------------------------------------------------
# Verdicts
# ---------------------------------------------------------------------------

def evaluate(answers: dict) -> tuple[list[str], list[str]]:
    """Return (failed_rules, warned_rules) for one file's answers."""
    failed: list[str] = []
    warned: list[str] = []

    file_kind = answers.get("file_kind")
    kind_choice = file_kind["choice"] if file_kind else None
    kind_confidence = file_kind["confidence"] if file_kind else 0.0
    kind_hit = kind_choice in ("run_record", "status_report")
    records_runs = answers.get("records_runs", {}).get("noul", 0.0)
    status_narrative = answers.get("status_narrative", {}).get("noul", 0.0)

    run_record_fail = (
        (kind_hit and kind_confidence >= FAIL_CONFIDENCE)
        or records_runs >= FAIL_PROBABILITY
        or status_narrative >= FAIL_PROBABILITY
    )
    run_record_warn = (
        (kind_hit and kind_confidence >= WARN_PROBABILITY)
        or records_runs >= WARN_PROBABILITY
        or status_narrative >= WARN_PROBABILITY
    )
    if run_record_fail:
        failed.append("run-record")
    elif run_record_warn:
        warned.append("run-record")

    if "decision_residue" in answers:
        decision_residue = answers["decision_residue"]["noul"]
        if decision_residue >= FAIL_PROBABILITY:
            failed.append("decision-residue")
        elif decision_residue >= WARN_PROBABILITY:
            warned.append("decision-residue")

    if "workload_named" in answers:
        workload = answers["workload_named"]
        if workload["choice"] != "none":
            if workload["confidence"] >= FAIL_CONFIDENCE:
                failed.append("workload-named")
            else:
                warned.append("workload-named")

    return failed, warned


REMEDIATION = {
    "run-record": (
        "This file reads as a run record or status report: dates, seeds, "
        "hashes, host names, or a narrative of what was done and what is "
        "next. Run records belong in external storage, not in the "
        "repository. Remove the file or rewrite it as reference material "
        "with no run-specific content."
    ),
    "decision-residue": (
        "This file describes a rejected alternative, a change made in "
        "answer to a reviewer, or an earlier name for something. Describe "
        "only what the thing is now; provenance belongs in git history."
    ),
    "workload-named": (
        "add the name to WORKLOAD_NAMES in scripts/custom-lints.py so the "
        "line-level rule catches it next time, then remove the reference."
    ),
}


def _format_signal(path: str, answers: dict) -> str:
    parts = []
    file_kind = answers.get("file_kind")
    if file_kind:
        parts.append(f"file_kind={file_kind['choice']} (confidence={file_kind['confidence']:.2f})")
    if "records_runs" in answers:
        parts.append(f"records_runs={answers['records_runs']['noul']:.2f}")
    if "status_narrative" in answers:
        parts.append(f"status_narrative={answers['status_narrative']['noul']:.2f}")
    if "decision_residue" in answers:
        parts.append(f"decision_residue={answers['decision_residue']['noul']:.2f}")
    if "workload_named" in answers:
        workload = answers["workload_named"]
        parts.append(f"workload_named={workload['choice']} (confidence={workload['confidence']:.2f})")
    return f"{path}: {' '.join(parts)}"


# ---------------------------------------------------------------------------
# Baseline
# ---------------------------------------------------------------------------

def load_baseline(repo_root: Path) -> dict[str, list[str]]:
    path = repo_root / SEMANTIC_BASELINE_PATH
    if not path.exists():
        return {}
    with path.open() as f:
        data = json.load(f)
    if not isinstance(data, dict):
        print(f"warning: {path} is not a JSON object, ignoring baseline", file=sys.stderr)
        return {}
    return {k: list(v) for k, v in data.items()}


def save_baseline(repo_root: Path, baseline: dict[str, list[str]]) -> None:
    path = repo_root / SEMANTIC_BASELINE_PATH
    pruned = {k: sorted(v) for k, v in baseline.items() if v}
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        json.dump(pruned, f, indent=2, sort_keys=True)
        f.write("\n")


def stale_baseline_entries(
    baseline: dict[str, list[str]],
    still_baselined: set[tuple[str, str]],
    judged: set[str],
) -> list[tuple[str, str]]:
    """A baseline entry is stale only if this run actually judged its file and
    the violation didn't reappear. `--changed-from` judges a subset of files,
    so a baseline entry for a file outside that subset is neither confirmed
    nor cleared and must not be reported as fixed.
    """
    stale = []
    for rule_name, paths in baseline.items():
        for path in paths:
            if path not in judged:
                continue
            if (rule_name, path) not in still_baselined:
                stale.append((rule_name, path))
    return stale


# ---------------------------------------------------------------------------
# File selection
# ---------------------------------------------------------------------------

def all_tracked_files(repo_root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    )
    return [p for p in result.stdout.split("\0") if p]


def changed_files(repo_root: Path, rev: str) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", "-z", "--find-renames", "--diff-filter=AMR", rev, "HEAD"],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    )
    return [p for p in result.stdout.split("\0") if p]


def select_files(repo_root: Path, candidates: list[str]) -> list[str]:
    selected = []
    for path in candidates:
        if path in SKIP_PATHS:
            continue
        if not (repo_root / path).is_file():
            continue
        if not _is_text_file(path):
            continue
        selected.append(path)
    return selected


# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

def _dump_row(path: str, question_id: str, answer: dict) -> list[str]:
    if "noul" in answer:
        return [path, question_id, str(answer["noul"]), ""]
    if "choice" in answer:
        return [path, question_id, str(answer["choice"]), str(answer.get("confidence", ""))]
    return [path, question_id, json.dumps(answer), ""]


def run(
    repo_root: Path,
    files: list[str],
    baseline: dict[str, list[str]],
    post: Callable[[str, dict, bytes], bytes] = _http_post,
    cache: dict | None = None,
    dump_rows: list | None = None,
) -> tuple[
    list[tuple[str, str, dict]],
    list[tuple[str, str, dict]],
    set[tuple[str, str]],
    dict,
    list[tuple[str, str]],
    set[str],
]:
    """Judge every file. Returns (new_failures, new_warnings, still_baselined,
    usage_totals, errors, judged).

    `new_failures`/`new_warnings` are (rule, path, answers) triples. `judged`
    holds every path this call got an answer for; callers use it to scope
    baseline comparisons to files that were actually checked this run.
    `cache`, when given, is mutated in place, so a second call over the
    same files with the same cache makes no new network calls. A file whose
    call raises (a state too large for the model's budget, a network fault,
    or a response that doesn't match the documented shape) is recorded in
    `errors` as (path, message) instead of aborting the rest of the sweep.
    """
    if cache is None:
        cache = {}
    usage_totals: dict[str, int] = {}
    new_failures: list[tuple[str, str, dict]] = []
    new_warnings: list[tuple[str, str, dict]] = []
    still_baselined: set[tuple[str, str]] = set()
    errors: list[tuple[str, str]] = []
    judged: set[str] = set()

    for path in files:
        try:
            answers = judge_file(repo_root, path, cache, post, usage_totals)
        except (JevHTTPError, OSError) as error:
            errors.append((path, str(error)))
            continue
        judged.add(path)
        if dump_rows is not None:
            for question_id, answer in answers.items():
                dump_rows.append(_dump_row(path, question_id, answer))
        failed_rules, warned_rules = evaluate(answers)
        for rule_name in failed_rules:
            if path in baseline.get(rule_name, []):
                still_baselined.add((rule_name, path))
            else:
                new_failures.append((rule_name, path, answers))
        for rule_name in warned_rules:
            new_warnings.append((rule_name, path, answers))

    return new_failures, new_warnings, still_baselined, usage_totals, errors, judged


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _print_usage(usage_totals: dict) -> None:
    print(
        f"usage: {usage_totals.get('input_tokens', 0)} input tokens, "
        f"{usage_totals.get('output_tokens', 0)} output tokens"
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
    )
    parser.add_argument("--changed-from", metavar="REV")
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--update-baseline", action="store_true")
    parser.add_argument("--dump", type=Path, help="Write a raw answer per file/question as TSV; do not commit it.")
    args = parser.parse_args(argv)

    api_key = os.environ.get("TYPESAFE_API_KEY")
    if not api_key:
        print("semantic lints skipped: TYPESAFE_API_KEY is not set")
        return 0

    if bool(args.all) == bool(args.changed_from):
        parser.error("pass exactly one of --all or --changed-from REV")

    root = args.repo_root.resolve()
    candidates = all_tracked_files(root) if args.all else changed_files(root, args.changed_from)
    files = select_files(root, candidates)
    baseline = load_baseline(root)
    cache = load_cache(root)
    dump_rows: list | None = [] if args.dump else None

    new_failures, new_warnings, still_baselined, usage_totals, errors, judged = run(
        root, files, baseline, post=_http_post, cache=cache, dump_rows=dump_rows,
    )
    save_cache(root, cache)

    if args.dump:
        with (root / args.dump).open("w") as f:
            f.write("path\tquestion\tvalue\tconfidence\n")
            for row in dump_rows:
                f.write("\t".join(row) + "\n")

    if args.update_baseline:
        # A baseline entry for a file this run didn't judge (--changed-from
        # skips most of the tree) carries forward unchanged; only a judged
        # file's entries are confirmed, dropped, or newly added.
        full: dict[str, list[str]] = {}
        for rule_name, paths in baseline.items():
            full[rule_name] = [p for p in paths if p not in judged or (rule_name, p) in still_baselined]
        for rule_name, path, _ in new_failures:
            full.setdefault(rule_name, []).append(path)
        save_baseline(root, full)
        count = sum(len(v) for v in full.values())
        print(f"semantic baseline updated: {count} known violation(s) in {SEMANTIC_BASELINE_PATH}")
        if errors:
            print(f"warning: {len(errors)} file(s) could not be judged and are not reflected:", file=sys.stderr)
            for path, message in errors:
                print(f"  {path}: {message}", file=sys.stderr)
        _print_usage(usage_totals)
        return 0

    stale = stale_baseline_entries(baseline, still_baselined, judged)

    if new_failures or stale or errors:
        print(
            f"semantic lints failed with {len(new_failures) + len(stale) + len(errors)} issue(s):",
            file=sys.stderr,
        )
        by_rule: dict[str, list[tuple[str, dict]]] = {}
        for rule_name, path, answers in new_failures:
            by_rule.setdefault(rule_name, []).append((path, answers))
        for rule_name in sorted(by_rule):
            print(f"\n  [{rule_name}] {REMEDIATION[rule_name]}", file=sys.stderr)
            for path, answers in by_rule[rule_name]:
                print(f"    {_format_signal(path, answers)}", file=sys.stderr)
        if stale:
            print(
                f"\n  [stale-baseline] These baseline entries no longer match a "
                f"violation. The underlying file was fixed; remove them from "
                f"{SEMANTIC_BASELINE_PATH}.",
                file=sys.stderr,
            )
            for rule_name, path in stale:
                print(f"    [{rule_name}] {path}", file=sys.stderr)
        if errors:
            print(
                f"\n  [judge-error] Jev could not judge this file (state too large "
                f"for its budget, a network fault, or a response that didn't match "
                f"the documented shape). Investigate and rerun; a file that never "
                f"gets judged is a silent gap in this check.",
                file=sys.stderr,
            )
            for path, message in errors:
                print(f"    {path}: {message}", file=sys.stderr)
        _print_usage(usage_totals)
        return 1

    for rule_name, path, answers in new_warnings:
        print(f"warning: [{rule_name}] {REMEDIATION[rule_name]}", file=sys.stderr)
        print(f"  {_format_signal(path, answers)}", file=sys.stderr)

    print(
        f"semantic lints passed ({len(files)} files, {len(still_baselined)} baselined, "
        f"{len(new_warnings)} warnings)"
    )
    _print_usage(usage_totals)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
