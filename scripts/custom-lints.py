#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Custom lints for the Harmony repository.

Enforces architectural boundaries, naming conventions, file placement, CI
workflow rules, and vocabulary constraints. Known violations are recorded in
a baseline file; new violations fail the check.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Sequence


# ---------------------------------------------------------------------------
# Workload-specific terms that must not appear in workload-agnostic code.
# ---------------------------------------------------------------------------

# Game titles and ROM identifiers.
GAME_NAMES = [
    r"nes",
    r"mario",
    r"super\s+mario\s+bros",
    r"smb",
    r"metroid",
    r"mega\s*man(?:\s*\d+)?",
    r"mm2",
    r"zelda",
    r"tetris",
    r"nova",
]

# Emulator and runtime names.
EMULATOR_NAMES = [
    r"quicknes",
    r"quick_nes",
    r"tetanes",
    r"libretro",
]

# Distributed system and database names.
SYSTEM_NAMES = [
    r"etcd",
    r"postgres(?:ql)?",
    r"cockroach(?:db)?",
    r"foundationdb",
    r"k3s",
    r"faultlab",
    r"fault[-_ ]library",
]

WORKLOAD_NAMES = GAME_NAMES + EMULATOR_NAMES + SYSTEM_NAMES

GAME_VOCABULARY = [
    r"game",
]


def _word_pattern(terms: list[str]) -> re.Pattern[str]:
    joined = "|".join(terms)
    return re.compile(rf"\b(?:{joined})\b", re.IGNORECASE)


WORKLOAD_NAME_RE = _word_pattern(WORKLOAD_NAMES)
GAME_VOCAB_RE = _word_pattern(GAME_VOCABULARY)

# Personal names and role titles that should not appear in code or docs.
PERSONAL_REFERENCE_RE = re.compile(r"\b(?:Paul|integrator)\b", re.IGNORECASE)

# Task-number references (task 42, task-42, (task 42), etc.).
TASK_REFERENCE_RE = re.compile(r"\btask[-_ ]?\d+\b", re.IGNORECASE)

# Dead-code markers in Rust attributes.
DEAD_CODE_RE = re.compile(
    r"#\[allow\(dead_code\)\]"
    r"|#\[allow\(unused"
    r"|#\[cfg\(dead\)\]"
    r"|// *TODO:? *remove\b"
    r"|// *FIXME:? *remove\b",
    re.IGNORECASE,
)

# Lazy numbered names: fn/struct/enum/trait/type whose name ends in a digit,
# and file stems whose name ends in a digit.  Excludes legitimate patterns
# (bit widths, architecture names, KVM ABI, version suffixes, hash and
# algorithm names, hardware device names).
#
# To add an entry to the allowlist: the name must be an external specification
# (a hardware part number, algorithm name, protocol version, register width).
# If the name is your own invention, choose a descriptive name instead.
NUMBERED_NAME_RE = re.compile(
    r"(?:pub\s+(?:\(crate\)\s+)?)?(?:fn|struct|enum|trait|type)\s+"
    r"(\w+[A-Za-z]\d+)\b"
)
NUMBERED_FILENAME_RE = re.compile(r"[A-Za-z]\d+$")
NUMBERED_NAME_ALLOWLIST = re.compile(
    # Bit-width suffixes (u8, u32, i64, f64, ...).
    r"(?:u|i|f)(?:8|16|32|64|128)$"
    # Architecture and hardware names.
    r"|(?:x86|arm64|Arm64|X86|Gicv3|gicv3)$"
    # Hardware device names (Pl011, Uart8250, ...).
    r"|(?:Pl011|Uart8250|8250)$"
    # KVM ABI types (sregs2, cpuid_entry2, xsave2, ...).
    r"|(?:sregs2|entry2|xsave2)$"
    # Hash and crypto algorithm names.
    r"|(?:[Ss]ha256|[Ss]ha512|[Cc]rc32|[Mm]d5|[Aa]es128|[Aa]es256|[Bb]lake3|CC_SHA256)$"
    # Explicit version suffixes (V2, _v3, ...).
    r"|[Vv]\d+$"
    # Byte-order and register-width encoding helpers (le32, be64, rd32, reg128, ...).
    r"|(?:le|be|rd|reg|array|hex)\d+$"
    # Algorithm names with numeric components.
    r"|(?:splitmix64|ceil_log2|log2)$"
    # CPUID leaf identifiers.
    r"|(?:leaf|subleaf)\d+$"
    # Numeric constants in test helpers.
    r"|(?:sum_to_\d+|modulo_\d+|pad\d+)$"
    # Hardware device and network adapter names.
    r"|(?:mlx5|mlx4|e1000|i40e|ixgbe|nvme)$"
    # Entropy/instruction scan identifiers (aa4, aa5, n6 scripts).
    r"|(?:n\d+|aa\d+)$"
)


# ---------------------------------------------------------------------------
# File filters
# ---------------------------------------------------------------------------

LINTABLE_EXTENSIONS = {
    ".rs", ".py", ".sh", ".c", ".h", ".toml", ".yml", ".yaml", ".json",
    ".md", ".txt", ".cfg",
}

BINARY_EXTENSIONS = {
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".wasm", ".o", ".so", ".a",
    ".dylib", ".lock",
}


def _is_rust_source(path: str) -> bool:
    return path.endswith(".rs")


def _is_lintable(path: str) -> bool:
    _, ext = os.path.splitext(path)
    return ext in LINTABLE_EXTENSIONS


def _is_any_file(_path: str) -> bool:
    return True


# ---------------------------------------------------------------------------
# Scopes
# ---------------------------------------------------------------------------

def _in_dir(path: str, prefix: str) -> bool:
    return path == prefix or path.startswith(prefix + "/") or path.startswith(prefix + os.sep)


def _in_dirs(path: str, prefixes: Sequence[str]) -> bool:
    return any(_in_dir(path, p) for p in prefixes)


def _consonance_core(path: str) -> bool:
    return _in_dir(path, "consonance") and not _in_dir(path, "consonance/harmony-linux")


SEARCHER_DIRS = [
    "dissonance/searcher",
]

# The guest Linux platform: the SDK and the platform build scripts.
# Workload-specific image recipes should live under workloads/, not here.
GUEST_LINUX_DIRS = [
    "consonance/harmony-linux",
    "harmony-linux",
]


# ---------------------------------------------------------------------------
# Rules
# ---------------------------------------------------------------------------

@dataclass
class Violation:
    rule: str
    path: str
    line: int
    text: str


@dataclass
class Rule:
    name: str
    description: str
    remediation: str
    pattern: re.Pattern[str]
    scope_fn: Callable[[str], bool]
    file_filter: Callable[[str], bool] = field(default=_is_rust_source)

    def applies(self, path: str) -> bool:
        return self.scope_fn(path) and self.file_filter(path)


# Paths to exclude from the dead-code and comment checks (generated, tests).
GENERATED_OR_TEST_PATTERNS = [
    re.compile(r"/tests?/"),
    re.compile(r"_tests?\.rs$"),
    re.compile(r"\.generated\."),
    re.compile(r"/benches?/"),
]


def _is_production_rust(path: str) -> bool:
    if not path.endswith(".rs"):
        return False
    return not any(p.search(path) for p in GENERATED_OR_TEST_PATTERNS)


RULES: list[Rule] = [
    Rule(
        name="searcher-no-workload-names",
        description="The dissonance searcher must not reference specific workloads.",
        remediation=(
            "The searcher is workload-agnostic. It must not name any specific game, "
            "emulator, database, or distributed system. Workload-specific logic belongs "
            "in workloads/. Use a generic term (target, subject, program) instead."
        ),
        pattern=WORKLOAD_NAME_RE,
        scope_fn=lambda p: _in_dirs(p, SEARCHER_DIRS),
        file_filter=_is_lintable,
    ),
    Rule(
        name="searcher-no-game-vocabulary",
        description="The dissonance searcher must not use 'game' vocabulary.",
        remediation=(
            "The searcher explores arbitrary deterministic programs, not just games. "
            "The word 'game' leaks a workload assumption into generic search code. "
            "Use 'target', 'subject', 'program', or 'workload' instead."
        ),
        pattern=GAME_VOCAB_RE,
        scope_fn=lambda p: _in_dirs(p, SEARCHER_DIRS),
    ),
    Rule(
        name="consonance-no-workload-names",
        description="Consonance core must not reference specific workloads.",
        remediation=(
            "Consonance is the deterministic execution engine. It must not know what "
            "program it runs. Workload-specific oracles, image builders, and config "
            "fragments belong in workloads/, not in consonance/. Move the code there."
        ),
        pattern=WORKLOAD_NAME_RE,
        scope_fn=_consonance_core,
        file_filter=_is_lintable,
    ),
    Rule(
        name="guest-linux-no-workload-names",
        description="Guest Linux platform must not reference specific workloads.",
        remediation=(
            "The harmony-linux directory is the guest Linux platform: kernel, SDK, "
            "paravirtual transport. Workload-specific image recipes, init scripts, "
            "and config fragments belong in workloads/, not here. Move them."
        ),
        pattern=WORKLOAD_NAME_RE,
        scope_fn=lambda p: _in_dirs(p, GUEST_LINUX_DIRS),
        file_filter=_is_lintable,
    ),
    Rule(
        name="no-dead-code-markers",
        description="No allow(dead_code) or unused suppression in production code.",
        remediation=(
            "Dead code must be deleted, not annotated. If the compiler says it is "
            "unused, remove it. If it is used only in tests, move it to the test module."
        ),
        pattern=DEAD_CODE_RE,
        scope_fn=lambda p: not _in_dir(p, "spikes"),
        file_filter=_is_production_rust,
    ),
    Rule(
        name="no-task-references",
        description="Code and docs must not reference task numbers.",
        remediation=(
            "Do not reference 'task N' in code, comments, or documentation. "
            "Rewrite without referencing the work item that triggered the "
            "change. Provenance belongs in git history and pull requests."
        ),
        pattern=TASK_REFERENCE_RE,
        scope_fn=lambda _: True,
        file_filter=_is_lintable,
    ),
    Rule(
        name="no-personal-references",
        description="Code and docs must not reference people by name or role title.",
        remediation=(
            "Do not reference people by name or use the word 'integrator' in "
            "code or documentation. Rewrite without referencing the person "
            "involved. Decisions belong to the project, not to an individual."
        ),
        pattern=PERSONAL_REFERENCE_RE,
        scope_fn=lambda p: not p.startswith("CONTRIBUTING") and not p == "LICENSE",
        file_filter=_is_lintable,
    ),
]


# ---------------------------------------------------------------------------
# Comment density check
# ---------------------------------------------------------------------------

COMMENT_LINE_RE = re.compile(r"^\s*//")
DOC_COMMENT_RE = re.compile(r"^\s*///|^\s*//!")
BLANK_LINE_RE = re.compile(r"^\s*$")


def check_comment_density(
    repo_root: Path, files: list[str], max_ratio: float = 0.30,
) -> list[Violation]:
    """Flag Rust files where comments exceed max_ratio of non-blank lines.

    Doc comments (/// and //!) are excluded — they serve the public API.
    Only plain // comments count.
    """
    violations = []
    for rel_path in files:
        if not _is_production_rust(rel_path):
            continue
        abs_path = repo_root / rel_path
        if not abs_path.is_file():
            continue
        try:
            lines = abs_path.read_text(errors="replace").splitlines()
        except OSError:
            continue

        non_blank = 0
        plain_comments = 0
        for line in lines:
            if BLANK_LINE_RE.match(line):
                continue
            non_blank += 1
            if COMMENT_LINE_RE.match(line) and not DOC_COMMENT_RE.match(line):
                plain_comments += 1

        if non_blank < 20:
            continue
        ratio = plain_comments / non_blank
        if ratio > max_ratio:
            violations.append(Violation(
                rule="comment-density",
                path=rel_path,
                line=0,
                text=f"{plain_comments}/{non_blank} non-blank lines are plain comments "
                     f"({ratio:.0%} > {max_ratio:.0%})",
            ))
    return violations


# ---------------------------------------------------------------------------
# Numbered-name check
# ---------------------------------------------------------------------------


# Milestone identifiers anywhere in a filename (m0, m1, ..., m9).
MILESTONE_FILENAME_RE = re.compile(r"(?:^|[-_])m\d+(?:[-_.]|$)")


def check_numbered_names(repo_root: Path, files: list[str]) -> list[Violation]:
    """Flag fn/struct/enum/trait/type names and file stems ending in a digit,
    and filenames containing milestone identifiers."""
    violations = []

    # Check file stems.
    for rel_path in files:
        stem = os.path.splitext(os.path.basename(rel_path))[0]
        if "." in stem:
            stem = os.path.splitext(stem)[0]
        if MILESTONE_FILENAME_RE.search(stem):
            violations.append(Violation(
                rule="no-milestone-names",
                path=rel_path,
                line=0,
                text=f"file stem '{stem}' contains a milestone identifier",
            ))
        elif NUMBERED_FILENAME_RE.search(stem) and not NUMBERED_NAME_ALLOWLIST.search(stem):
            violations.append(Violation(
                rule="no-numbered-names",
                path=rel_path,
                line=0,
                text=f"file stem '{stem}' ends in a digit",
            ))

    # Check Rust identifiers.
    for rel_path in files:
        if not _is_rust_source(rel_path):
            continue
        abs_path = repo_root / rel_path
        if not abs_path.is_file():
            continue
        try:
            lines = abs_path.read_text(errors="replace").splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines, 1):
            m = NUMBERED_NAME_RE.search(line)
            if m and not NUMBERED_NAME_ALLOWLIST.search(m.group(1)):
                violations.append(Violation(
                    rule="no-numbered-names",
                    path=rel_path,
                    line=i,
                    text=line.strip(),
                ))
    return violations


# ---------------------------------------------------------------------------
# Workload-specific files in the wrong directory
# ---------------------------------------------------------------------------

MISPLACED_WORKLOAD_FILE_RE = re.compile(
    r"(?:^|[-_/])"
    r"(?:game|nes|nova|smb|mario|metroid|tetanes|tetris|postgres|etcd|cockroach|k3s|docker|faultlab|fault[-_]library)"
    r"(?:[-_./]|$)",
    re.IGNORECASE,
)

MISPLACED_ALLOWLIST = {
    # Platform scripts that are genuinely workload-agnostic despite their name.
}


def check_misplaced_workload_files(files: list[str]) -> list[Violation]:
    """Flag workload-specific files living inside harmony-linux."""
    violations = []
    for path in files:
        if not _in_dirs(path, GUEST_LINUX_DIRS):
            continue
        if path in MISPLACED_ALLOWLIST:
            continue
        basename = os.path.basename(path)
        if MISPLACED_WORKLOAD_FILE_RE.search(basename):
            violations.append(Violation(
                rule="misplaced-workload-file",
                path=path,
                line=0,
                text=f"workload-specific file in platform directory",
            ))
    return violations


# ---------------------------------------------------------------------------
# CI workflow name prefix check
# ---------------------------------------------------------------------------

CI_WORKFLOW_PREFIXES = ["Acceptance", "Benchmarks", "Checks", "Nightly", "Release", "Smoke"]

PR_JOB_MAX_TIMEOUT_MINUTES = 15
PR_WORKFLOWS = {
    ".github/workflows/quality.yml": "Checks",
    ".github/workflows/nightly.yml": "Checks",
    ".github/workflows/product-smoke.yml": "Smoke",
}
CI_NAME_TERMS = {
    "API", "CLI", "CPU", "KVM", "NES", "STB", "VM",
    "Consonance", "Dissonance", "Docker", "Go", "Harmony", "Intel",
    "K3s", "Kani", "Linux", "Miri", "Nova", "PostgreSQL", "QuickNES",
}
CI_GENERIC_NAMES = {"gates", "products", "quality", "checks", "smoke", "test", "tests", "job", "report"}


def _sentence_case_name(name: str) -> bool:
    """Check static display text without interpreting matrix expressions."""
    static = re.sub(r"\$\{\{.*?\}\}", "", name)
    words = re.findall(r"[A-Za-z][A-Za-z0-9]*", static)
    if not words or name.strip().lower() in CI_GENERIC_NAMES or " / " in name:
        return False
    for index, word in enumerate(words):
        canonical = next((term for term in CI_NAME_TERMS if term.lower() == word.lower()), None)
        if canonical is not None:
            if word != canonical:
                return False
            continue
        expected = word[:1].upper() + word[1:].lower() if index == 0 else word.lower()
        if word != expected:
            return False
    return True

# Event names that cannot be a pull-request run.
NON_PR_EVENTS = {"push", "schedule", "workflow_dispatch"}

EVENT_NAME_RE = re.compile(r"(?:github\.)?event_name\b")
EVENT_EQUALITY_RE = re.compile(
    r"(?:github\.)?event_name\s*==\s*(['\"])([^'\"]+)\1"
)


def _split_condition(expression: str, operator: str) -> list[str]:
    """Split a boolean expression at top-level operators."""
    parts = []
    start = 0
    depth = 0
    quote = ""
    index = 0
    while index < len(expression):
        char = expression[index]
        if quote:
            if char == quote and (index == 0 or expression[index - 1] != "\\"):
                quote = ""
            index += 1
            continue
        if char in "'\"":
            quote = char
            index += 1
            continue
        if char == "(":
            depth += 1
        elif char == ")":
            depth = max(0, depth - 1)
        elif depth == 0 and expression.startswith(operator, index):
            parts.append(expression[start:index])
            index += len(operator)
            start = index
            continue
        index += 1
    parts.append(expression[start:])
    return parts


def _strip_outer_parentheses(expression: str) -> str:
    expression = expression.strip()
    while expression.startswith("(") and expression.endswith(")"):
        depth = 0
        closes_at = None
        quote = ""
        for index, char in enumerate(expression):
            if quote:
                if char == quote and (index == 0 or expression[index - 1] != "\\"):
                    quote = ""
                continue
            if char in "'\"":
                quote = char
            elif char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    closes_at = index
                    break
        if closes_at != len(expression) - 1:
            break
        expression = expression[1:-1].strip()
    return expression


def _condition_excludes_pr(expression: str) -> bool:
    """Prove that every true branch names a non-PR event.

    This intentionally accepts only positive equality guards. Expressions with
    a negative comparison or an unguarded OR branch remain subject to the
    timeout lint.
    """
    expression = _strip_outer_parentheses(expression)
    disjuncts = _split_condition(expression, "||")
    if len(disjuncts) > 1:
        return all(_condition_excludes_pr(part) for part in disjuncts)
    conjuncts = _split_condition(expression, "&&")
    if len(conjuncts) > 1:
        return any(_condition_excludes_pr(part) for part in conjuncts)
    match = EVENT_EQUALITY_RE.fullmatch(expression)
    return match is not None and match.group(2) in NON_PR_EVENTS


def _job_skips_pr(job_if: str) -> bool:
    """Return True only if the `if:` guard proves the job excludes PRs."""
    if not job_if:
        return False
    normalized = job_if.replace("${{", "").replace("}}", "").strip()
    if not EVENT_NAME_RE.search(normalized):
        return False
    return _condition_excludes_pr(normalized)


def _parse_workflow(abs_path: Path) -> dict:
    try:
        import yaml
    except ImportError as error:
        raise ValueError("workflow validation requires PyYAML; install PyYAML==6.0.3") from error

    class UniqueKeyLoader(yaml.SafeLoader):
        pass

    def mapping(loader, node, deep=False):
        result = {}
        for key_node, value_node in node.value:
            key = loader.construct_object(key_node, deep=deep)
            if key in result:
                raise ValueError(f"duplicate YAML key: {key}")
            result[key] = loader.construct_object(value_node, deep=deep)
        return result

    UniqueKeyLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, mapping)
    try:
        data = yaml.load(abs_path.read_text(errors="replace"), Loader=UniqueKeyLoader)
    except (yaml.YAMLError, ValueError) as error:
        raise ValueError(f"invalid workflow YAML: {error}") from error
    if not isinstance(data, dict) or not isinstance(data.get("jobs"), dict):
        raise ValueError("workflow must be a mapping with a jobs mapping")
    for name, job in data["jobs"].items():
        if not isinstance(job, dict):
            raise ValueError(f"job '{name}' must be a mapping")
        if "steps" in job and not isinstance(job["steps"], list):
            raise ValueError(f"job '{name}' steps must be a list")
        if any(not isinstance(step, dict) for step in job.get("steps", [])):
            raise ValueError(f"job '{name}' steps must contain mappings")
    return data


def check_workflow_rules(repo_root: Path, files: list[str]) -> list[Violation]:
    """Validate the complete tracked-file inventory, not a changed-file subset."""
    violations = []
    nes_manifest = "benchmarks/search/nightly.json"
    nes_workflow = ".github/workflows/nova-nightly.yml"
    if nes_manifest in files or (repo_root / nes_manifest).is_file():
        if nes_workflow not in files or not (repo_root / nes_workflow).is_file():
            violations.append(Violation("ci-nes-case-jobs", nes_workflow, 0,
                "the registered public NES manifest requires its tracked owning workflow"))
    for rel_path in files:
        if not rel_path.startswith(".github/workflows/") or not rel_path.endswith((".yml", ".yaml")):
            continue
        abs_path = repo_root / rel_path
        if not abs_path.is_file():
            continue

        try:
            data = _parse_workflow(abs_path)
        except (OSError, ValueError) as error:
            violations.append(Violation("ci-workflow-parse", rel_path, 0, str(error)))
            continue
        name = str(data.get("name", "")).strip()
        if not name:
            violations.append(Violation(
                rule="ci-workflow-prefix",
                path=rel_path,
                line=0,
                text="workflow file has no 'name:' field",
            ))
        else:
            prefix = name.split("/")[0].strip() if "/" in name else name
            flat = name in {"Checks", "Smoke"}
            if prefix not in CI_WORKFLOW_PREFIXES or (not flat and not re.fullmatch(r"\S+ / \S.*", name)):
                violations.append(Violation(
                    rule="ci-workflow-prefix",
                    path=rel_path,
                    line=0,
                    text=f"workflow name '{name}' does not start with an allowed prefix",
                ))
            elif prefix in {"Checks", "Smoke"} and not flat:
                violations.append(Violation("ci-display-name", rel_path, 0,
                    "Checks and Smoke workflow names must be the category alone; descriptive subjects belong on jobs"))
            elif not flat and not _sentence_case_name(name.split(" / ", 1)[1]):
                violations.append(Violation("ci-display-name", rel_path, 0,
                    f"workflow subject must be descriptive sentence case: {name}"))
        for job_id, job in data["jobs"].items():
            job_name = job.get("name")
            if not isinstance(job_name, str) or not _sentence_case_name(job_name):
                violations.append(Violation("ci-display-name", rel_path, 0,
                    f"job '{job_id}' needs a descriptive sentence-case display name (capitalize proper names and registered acronyms only)"))

        triggers = data.get("on", data.get(True, {}))
        if isinstance(triggers, str):
            triggers = {triggers: None}
        if isinstance(triggers, list) and all(isinstance(t, str) for t in triggers):
            triggers = {t: None for t in triggers}
        if not isinstance(triggers, dict) or not triggers or not all(isinstance(t, str) for t in triggers):
            violations.append(Violation("ci-workflow-parse", rel_path, 0, "workflow has no valid triggers"))
            continue
        category = str(data.get("name", "")).split(" / ")[0]
        if category in {"Acceptance", "Benchmarks", "Nightly"}:
            forbidden = set(triggers) - {"schedule", "workflow_dispatch", "workflow_call"}
        elif category in {"Checks", "Smoke"}:
            forbidden = set(triggers) & {"schedule"}
        else:
            forbidden = set()
        if forbidden:
            violations.append(Violation("ci-workflow-triggers", rel_path, 0,
                f"{category} workflow has forbidden triggers: {', '.join(sorted(forbidden))}"))
        if rel_path == ".github/workflows/nova-nightly.yml" or str(data.get("name", "")) == "Benchmarks / NES":
            violations.extend(check_nes_job_coverage(repo_root, rel_path, data))
        if not set(triggers) & {"pull_request", "pull_request_target", "merge_group"}:
            continue
        if PR_WORKFLOWS.get(rel_path) != category:
            violations.append(Violation("ci-pr-workflow-registration", rel_path, 0,
                "PR workflows must use a registered path and category; register new checks explicitly"))
        if category == "Smoke" or rel_path == ".github/workflows/product-smoke.yml":
            violations.extend(check_pr_smoke_routing(rel_path, data))
        for job_name, job in (data.get("jobs") or {}).items():
            if not isinstance(job, dict):
                continue
            timeout = job.get("timeout-minutes")
            job_if = str(job.get("if", ""))
            if _job_skips_pr(job_if):
                violations.append(Violation("ci-pr-only-jobs", rel_path, 0,
                    f"job '{job_name}' belongs in a separate schedule/manual workflow"))
                continue
            commands = "\n".join(str(step.get("run", "")) for step in job.get("steps", []) if isinstance(step, dict))
            if re.search(r"\bcargo(?:\s+\+\S+)?\s+(?:mutants|llvm-cov)\b|\bscripts/coverage\.sh\b", commands):
                violations.append(Violation("ci-pr-extended-validation", rel_path, 0,
                    f"job '{job_name}' runs mutation or coverage; move it to Nightly"))
            # Every automatic PR job must declare its bound. Treat a missing
            # or malformed value as a violation instead of silently allowing
            # an unbounded runner.
            if (
                isinstance(timeout, (int, float))
                and not isinstance(timeout, bool)
                and 0 < timeout <= PR_JOB_MAX_TIMEOUT_MINUTES
            ):
                continue
            detail = (
                f"has timeout-minutes={timeout}"
                if timeout is not None
                else "has no timeout-minutes"
            )
            violations.append(Violation(
                rule="ci-pr-job-timeout",
                path=rel_path,
                line=0,
                text=f"job '{job_name}' {detail} (must set <= {PR_JOB_MAX_TIMEOUT_MINUTES} for PR jobs)",
            ))
    return violations


def check_pr_smoke_routing(path: str, workflow: dict) -> list[Violation]:
    """Keep automatic product smokes behind the tested consumer selector."""
    from ci_scope import SMOKES

    def problem(message):
        return [Violation("ci-pr-smoke-routing", path, 0, message)]

    if path != ".github/workflows/product-smoke.yml":
        return problem("PR smokes must be registered in product-smoke.yml and scripts/ci_scope.py")
    if workflow.get("name") != "Smoke":
        return problem("the registered product workflow must retain its Smoke category")
    jobs = workflow["jobs"]
    expected = set(SMOKES) | {"scope", "guest-qualification"}
    if set(jobs) != expected:
        return problem("product smoke jobs must match the selector's registered consumers and qualification check")
    scope = jobs["scope"]
    commands = "\n".join(str(step.get("run", "")) for step in scope.get("steps", []))
    if "python3 scripts/ci_scope.py" not in commands or not set(SMOKES).issubset(scope.get("outputs", {})):
        return problem("scope must execute ci_scope.py and publish every consumer selection")
    for name in SMOKES:
        job = jobs[name]
        guard = str(job.get("if", "")).replace("${{", "").replace("}}", "").strip()
        if job.get("needs") not in ("scope", ["scope"]) or guard != f"needs.scope.outputs.{name} == 'true'" or "strategy" in job:
            return problem(f"smoke '{name}' must be one job gated by its scope output")
    return []


def check_nes_job_coverage(repo_root: Path, path: str, workflow: dict) -> list[Violation]:
    """Require the public roster to map one-to-one to independently runnable jobs."""
    def problem(message):
        return [Violation("ci-nes-case-jobs", path, 0, message)]

    try:
        suite = json.loads((repo_root / "benchmarks/search/nightly.json").read_text())
        expected = [case["id"] for case in suite["cases"]]
    except (OSError, ValueError, KeyError, TypeError) as error:
        return problem(f"cannot read public NES case manifest: {error}")
    if not expected or len(expected) != len(set(expected)):
        return problem("public NES manifest must contain unique, nonempty cases")
    actual = []
    campaign_jobs = []
    for name, job in workflow["jobs"].items():
        commands = "\n".join(str(step.get("run", "")) for step in job.get("steps", []) if isinstance(step, dict))
        if "eval.py run" not in commands:
            continue
        campaign_jobs.append(name)
        strategy = job.get("strategy", {})
        matrix = strategy.get("matrix", {})
        cases = matrix.get("case") if isinstance(matrix, dict) else None
        if (not isinstance(cases, list) or set(matrix) != {"case"} or not all(isinstance(case, str) for case in cases)
                or strategy.get("fail-fast") is not False
                or not re.search(r"--case\s+[\"']?\$\{\{\s*matrix\.case\s*\}\}", commands)
                or "benchmarks/search/nightly.json" not in commands):
            return problem(f"job '{name}' must run nightly.json with --case matrix.case, a static case matrix and fail-fast: false")
        actual.extend(cases)
    if sorted(actual) != sorted(expected):
        return problem("NES case matrices must cover every public manifest case exactly once")
    report = workflow["jobs"].get("report", {})
    needs = report.get("needs", [])
    if isinstance(needs, str):
        needs = [needs]
    if (str(report.get("if", "")).replace("${{", "").replace("}}", "").strip() != "always()"
            or not set(campaign_jobs).issubset(needs)):
        return problem("report must use always() and depend on every campaign matrix")
    return []


# ---------------------------------------------------------------------------
# Lab-notes rule: certain file patterns must not be tracked.
# ---------------------------------------------------------------------------

LAB_NOTE_PATTERNS = [
    re.compile(r"(?:^|/)lab[_-]?notes?/", re.IGNORECASE),
    re.compile(r"(?:^|/)reports?/.*\.md$", re.IGNORECASE),
    re.compile(r"(?:^|/)run[_-]\d+", re.IGNORECASE),
    re.compile(r"(?:^|/)campaign[_-]results?/", re.IGNORECASE),
]

LAB_NOTE_ALLOWLIST = {
    "benchmarks/search/results/README.md",
}


# ---------------------------------------------------------------------------
# Golden-output checks: hardcoded execution hashes and generated-file diffs
# ---------------------------------------------------------------------------

# Matches: expected_foo=<64-hex-char sha256> (hardcoded execution output hash).
GOLDEN_HASH_RE = re.compile(r"expected_\w+\s*=\s*[0-9a-f]{64}\b")

# Matches: cmp ... .generated. (byte-for-byte comparison against a committed
# generated reference file).
GOLDEN_CMP_RE = re.compile(r"\bcmp\b.*\.generated\.")

GOLDEN_OUTPUT_RE = re.compile(
    rf"(?:{GOLDEN_HASH_RE.pattern})|(?:{GOLDEN_CMP_RE.pattern})"
)

# Allowlist paths where golden-output comparisons are tolerable (build
# reproducibility, API surface contracts).
GOLDEN_OUTPUT_ALLOWLIST_RE = re.compile(
    r"MANIFEST\.sha256|public-api\.txt|versions\.lock"
)


def check_golden_outputs(repo_root: Path, files: list[str]) -> list[Violation]:
    violations = []
    for rel_path in files:
        if not _is_lintable(rel_path):
            continue
        if GOLDEN_OUTPUT_ALLOWLIST_RE.search(rel_path):
            continue
        abs_path = repo_root / rel_path
        if not abs_path.is_file():
            continue
        try:
            lines = abs_path.read_text(errors="replace").splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines, 1):
            if GOLDEN_OUTPUT_RE.search(line):
                violations.append(Violation(
                    rule="no-golden-output",
                    path=rel_path,
                    line=i,
                    text=line.strip(),
                ))
    return violations


# ---------------------------------------------------------------------------
# docs/ allowlist: only pre-approved files may live here
# ---------------------------------------------------------------------------

DOCS_ALLOWLIST = {
    "docs/ARCHITECTURE.md",
    "docs/DETERMINISM.md",
    "docs/EXPLORATION.md",
    "docs/HARDWARE-TESTING.md",
    "docs/PROTOCOL.md",
    "docs/TESTING.md",
    "docs/WORKFLOWS.md",
}


def check_docs_allowlist(files: list[str]) -> list[Violation]:
    violations = []
    for rel_path in files:
        if not rel_path.startswith("docs/"):
            continue
        if rel_path == str(BASELINE_PATH):
            continue
        if not rel_path.endswith(".md"):
            violations.append(Violation(
                rule="docs-markdown-only",
                path=rel_path,
                line=0,
                text="non-Markdown file in docs/",
            ))
            continue
        if rel_path in DOCS_ALLOWLIST:
            continue
        violations.append(Violation(
            rule="docs-allowlist",
            path=rel_path,
            line=0,
            text="file not in docs/ allowlist",
        ))
    return violations


# ---------------------------------------------------------------------------
# Top-level directory allowlist
# ---------------------------------------------------------------------------

TOPLEVEL_DIR_ALLOWLIST = {
    ".cargo",
    ".config",
    ".githooks",
    ".github",
    "benchmarks",
    "cli",
    "consonance",
    "dissonance",
    "docs",
    "scripts",
    "workloads",
}


def check_toplevel_dirs(files: list[str]) -> list[Violation]:
    violations = []
    seen_dirs: set[str] = set()
    for rel_path in files:
        if "/" not in rel_path:
            continue
        top = rel_path.split("/")[0]
        if top in TOPLEVEL_DIR_ALLOWLIST or top in seen_dirs:
            continue
        if top.startswith("."):
            # Dotfiles/dirs not in allowlist.
            pass
        seen_dirs.add(top)
        violations.append(Violation(
            rule="toplevel-dir-allowlist",
            path=top,
            line=0,
            text=f"top-level directory '{top}' is not in the allowlist",
        ))
    return violations


# ---------------------------------------------------------------------------
# .github directory: only .yml files allowed
# ---------------------------------------------------------------------------


def check_github_dir_files(files: list[str]) -> list[Violation]:
    violations = []
    for rel_path in files:
        if not rel_path.startswith(".github/"):
            continue
        if rel_path.endswith(".yml"):
            continue
        violations.append(Violation(
            rule="github-dir-yml-only",
            path=rel_path,
            line=0,
            text=f"non-.yml file in .github/",
        ))
    return violations


# ---------------------------------------------------------------------------
# Baseline — known violations that predate the lint.
# ---------------------------------------------------------------------------

BASELINE_PATH = Path("docs/custom-lints-baseline.json")


def load_baseline(repo_root: Path) -> dict[str, list[str]]:
    path = repo_root / BASELINE_PATH
    if not path.exists():
        return {}
    with path.open() as f:
        data = json.load(f)
    if not isinstance(data, dict):
        print(f"warning: {path} is not a JSON object, ignoring baseline", file=sys.stderr)
        return {}
    return {k: list(v) for k, v in data.items()}


def save_baseline(repo_root: Path, baseline: dict[str, list[str]]) -> None:
    path = repo_root / BASELINE_PATH
    pruned = {k: sorted(v) for k, v in baseline.items() if v}
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        json.dump(pruned, f, indent=2, sort_keys=True)
        f.write("\n")


# ---------------------------------------------------------------------------
# Checker
# ---------------------------------------------------------------------------

def tracked_files(repo_root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    )
    # This script defines the patterns it searches for, so it cannot lint itself.
    self_path = str(Path(__file__).resolve().relative_to(repo_root))
    return [p for p in result.stdout.split("\0") if p and p != self_path]


def check_content_rules(
    repo_root: Path, files: list[str], baseline: dict[str, list[str]]
) -> tuple[list[Violation], set[tuple[str, str]]]:
    """Return (new_violations, baseline (rule, key) pairs still present)."""
    new_violations: list[Violation] = []
    still_baselined: set[tuple[str, str]] = set()

    for rule in RULES:
        rule_baseline = set(baseline.get(rule.name, []))
        applicable = [f for f in files if rule.applies(f)]
        for rel_path in applicable:
            abs_path = repo_root / rel_path
            if not abs_path.is_file():
                continue
            try:
                lines = abs_path.read_text(errors="replace").splitlines()
            except OSError:
                continue
            for i, line in enumerate(lines, 1):
                if rule.pattern.search(line):
                    key = f"{rel_path}:{i}"
                    if key in rule_baseline:
                        still_baselined.add((rule.name, key))
                    else:
                        new_violations.append(
                            Violation(rule=rule.name, path=rel_path, line=i, text=line.strip())
                        )

    return new_violations, still_baselined


def check_lab_notes(files: list[str]) -> list[str]:
    violations = []
    for path in files:
        if path in LAB_NOTE_ALLOWLIST:
            continue
        for pattern in LAB_NOTE_PATTERNS:
            if pattern.search(path):
                violations.append(path)
                break
    return violations


def _violation_key(v: Violation) -> str:
    if v.line == 0:
        return f"{v.path}:0"
    return f"{v.path}:{v.line}"


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
    )
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help="Write the current violations as the new baseline.",
    )
    args = parser.parse_args(argv)
    root = args.repo_root.resolve()

    files = tracked_files(root)
    baseline = load_baseline(root)

    # Content rules (line-level pattern matches).
    new_violations, still_baselined = check_content_rules(root, files, baseline)

    # File-level checks.
    comment_violations = check_comment_density(root, files)
    misplaced_violations = check_misplaced_workload_files(files)
    numbered_violations = check_numbered_names(root, files)
    workflow_violations = check_workflow_rules(root, files)
    github_dir_violations = check_github_dir_files(files)
    golden_violations = check_golden_outputs(root, files)
    docs_violations = check_docs_allowlist(files)
    toplevel_violations = check_toplevel_dirs(files)
    lab_violations = check_lab_notes(files)

    # Merge file-level violations into the baseline system.
    all_file_violations = comment_violations + misplaced_violations + numbered_violations + workflow_violations + github_dir_violations + golden_violations + docs_violations + toplevel_violations
    new_file_violations: list[Violation] = []
    for v in all_file_violations:
        key = _violation_key(v)
        rule_baseline = set(baseline.get(v.rule, []))
        if key in rule_baseline and not v.rule.startswith("ci-"):
            still_baselined.add((v.rule, key))
        else:
            new_file_violations.append(v)

    errors: list[str] = []

    if args.update_baseline:
        ci_errors = [v for v in workflow_violations if v.rule.startswith("ci-")]
        if ci_errors:
            print("cannot baseline CI contract violations; fix the workflows first", file=sys.stderr)
            return 1
        full: dict[str, list[str]] = {}
        for v in new_violations + new_file_violations:
            full.setdefault(v.rule, []).append(_violation_key(v))
        for rule_name, key in still_baselined:
            full.setdefault(rule_name, []).append(key)
        save_baseline(root, full)
        count = sum(len(v) for v in full.values())
        print(f"baseline updated: {count} known violation(s) in {BASELINE_PATH}")
        return 0

    # Remediation text for non-Rule checks.
    REMEDIATION = {
        "comment-density": (
            "This file has too many plain // comments relative to its code. "
            "Comments should explain why, never what. Delete comments that "
            "restate the code or describe the current task."
        ),
        "misplaced-workload-file": (
            "This file has a workload-specific name but lives in a platform "
            "directory. Workload image recipes, init scripts, and config "
            "fragments belong in workloads/, not in consonance/harmony-linux/."
        ),
        "no-numbered-names": (
            "Function, type, and file names must not end in a digit. "
            "If the name refers to an external specification (hardware part "
            "number, algorithm, register width), add it to NUMBERED_NAME_ALLOWLIST "
            "in this script. Otherwise, choose a descriptive name."
        ),
        "no-milestone-names": (
            "File names must not contain milestone identifiers (m0, m1, m2, ...). "
            "Milestones are project-management ordering that does not belong in "
            "code or filenames. Name the file after what it does."
        ),
        "docs-markdown-only": (
            "The docs/ directory may only contain Markdown (.md) files. "
            "Configuration files belong next to the code that consumes them. "
            "Move this file to the appropriate location."
        ),
        "docs-allowlist": (
            "The docs/ directory has a fixed allowlist in DOCS_ALLOWLIST in "
            "this script. To add a new file, add its path to the allowlist "
            "and get project owner approval. Do not add files to docs/ without "
            "asking first."
        ),
        "no-golden-output": (
            "Tests must not hardcode an expected execution hash or compare "
            "output byte-for-byte against a committed generated file. These "
            "checks break whenever the implementation changes, even validly. "
            "To test determinism, run the same seed twice and assert the "
            "outputs match each other (self-consistency). Build-reproducibility "
            "checks (MANIFEST.sha256) and API surface snapshots (public-api.txt) "
            "are exempt."
        ),
        "toplevel-dir-allowlist": (
            "Only these top-level directories are allowed: "
            + ", ".join(sorted(TOPLEVEL_DIR_ALLOWLIST))
            + ". To add a new top-level directory, add it to "
            "TOPLEVEL_DIR_ALLOWLIST in this script and get project owner "
            "approval first."
        ),
        "github-dir-yml-only": (
            "The .github/ directory may only contain .yml workflow files. "
            "Scripts belong in scripts/ at the repo root. Move this file there "
            "and update any workflow steps that reference it."
        ),
        "ci-pr-job-timeout": (
            f"Automatic per-PR jobs must not exceed {PR_JOB_MAX_TIMEOUT_MINUTES} minutes. "
            "Very short workloads are smoke tests (Smoke category). Long-running "
            "workloads are acceptance tests that run on an off-hours schedule "
            "(Acceptance category). If the job must run on every PR, split it into "
            "smaller pieces or move the expensive part behind a schedule or "
            "workflow_dispatch trigger. Comment-based timeout exceptions are not allowed."
        ),
        "ci-workflow-parse": "Install PyYAML==6.0.3 and fix malformed or duplicate YAML fields; workflow checks never silently skip parsing.",
        "ci-workflow-triggers": "Acceptance, Benchmarks and Nightly allow only schedule/manual/reusable triggers. Checks and Smoke cannot be scheduled.",
        "ci-pr-only-jobs": "Keep nightly/manual jobs out of PR workflows, including jobs hidden behind event guards.",
        "ci-pr-extended-validation": "Run coverage and mutation in Nightly workflows; a short timeout does not make them PR checks.",
        "ci-pr-workflow-registration": "Register new PR workflows and their category in PR_WORKFLOWS; route product smokes through the existing selector.",
        "ci-display-name": "Use descriptive sentence-case subjects, preserving registered proper names/acronyms. Checks and Smoke are category-only workflow names; job names supply the flat display subject. Do not use generic names such as Gates, Products or Quality.",
        "ci-pr-smoke-routing": "Register PR product smokes through the tested consumer selector; do not add independent broad triggers or unbounded matrices.",
        "ci-nes-case-jobs": "Map every public NES manifest case exactly once to the case matrix, select it with --case, disable fail-fast, and retain an always-running report.",
        "ci-workflow-prefix": (
            "Every GitHub Actions workflow name must start with one of the "
            "defined categories: Acceptance, Benchmarks, Checks, Nightly, Release, Smoke. "
            "Use 'Checks' or 'Smoke' with descriptive job names, or 'Category / Description' for other workflows. If none of these "
            "categories fit, ask the project owner whether to create a new one."
        ),
        "lab-notes-not-tracked": (
            "Lab notes, run reports, and campaign results must not be checked "
            "into the repository. They belong in external storage. Remove the "
            "file from version control."
        ),
    }

    all_violations = new_violations + new_file_violations
    for path in lab_violations:
        all_violations.append(Violation(
            rule="lab-notes-not-tracked", path=path, line=0, text="",
        ))

    # Group violations by rule for readable output.
    by_rule: dict[str, list[Violation]] = {}
    for v in all_violations:
        by_rule.setdefault(v.rule, []).append(v)

    stale: list[str] = []
    for rule_name, keys in baseline.items():
        for key in keys:
            if (rule_name, key) not in still_baselined:
                stale.append(f"  [{rule_name}] {key}")

    error_count = len(all_violations) + len(stale)
    if error_count > 0:
        print(
            f"custom lints failed with {error_count} issue(s):",
            file=sys.stderr,
        )
        for rule_name, violations in sorted(by_rule.items()):
            # Print the remediation once per rule.
            rule_obj = next((r for r in RULES if r.name == rule_name), None)
            guidance = rule_obj.remediation if rule_obj else REMEDIATION.get(rule_name, "")
            print(f"\n  [{rule_name}] {guidance}", file=sys.stderr)
            for v in violations:
                loc = f"{v.path}:{v.line}" if v.line else v.path
                suffix = f": {v.text}" if v.text else ""
                print(f"    {loc}{suffix}", file=sys.stderr)
        if stale:
            print(
                f"\n  [stale-baseline] These baseline entries no longer match a "
                f"violation. The underlying code was fixed; remove them from "
                f"{BASELINE_PATH}.",
                file=sys.stderr,
            )
            for s in stale:
                print(f"  {s}", file=sys.stderr)
        return 1

    baselined_count = len(still_baselined)
    suffix = f" ({baselined_count} baselined)" if baselined_count else ""
    print(f"custom lints passed{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
