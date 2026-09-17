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
    # Architectural x86 control register names.
    r"|(?:cr[02348])$"
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
# CI workflow rules. scripts/ci_contract.py is the registry of workflow names,
# owners, triggers and budgets; these checks hold the files to it.
# ---------------------------------------------------------------------------

PR_JOB_MAX_TIMEOUT_MINUTES = 15

# Events that can put a job in front of a pull request.
PR_EVENTS = {"pull_request", "pull_request_target", "merge_group"}

TRIGGER_WORDS_RE = re.compile(r"\b(scheduled|manual|nightly)\b", re.IGNORECASE)
BARE_ORDINAL_RE = re.compile(r"\(\s*\d+\s*\)")
MATRIX_EXPRESSION_RE = re.compile(r"\$\{\{.*?\}\}")
VARIANT_RE = re.compile(r"<[^>]+>")

# Stands in for a value only the runner knows.
UNKNOWN_VALUE = "\x00"

# The game runs natively through Dissonance and inside a Consonance VM through
# Harmony. Neither composition substitutes for the other.
NES_COMPOSITIONS_REQUIRED = ("Dissonance Workloads", "Harmony Workloads")


def _display_shape(name: str) -> str:
    """Collapse the values a matrix supplies at run time into one token."""
    return MATRIX_EXPRESSION_RE.sub(UNKNOWN_VALUE, str(name)).strip()


def _variant_pattern(registered: str) -> re.Pattern[str]:
    """Match a registered name against the values its variants can take."""
    return re.compile(".+".join(re.escape(part) for part in VARIANT_RE.split(registered)))


def _matrix_values(job: dict) -> dict[str, list]:
    matrix = (job.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        return {}
    values: dict[str, list] = {}
    include = matrix.get("include")
    if isinstance(include, list):
        for entry in include:
            if not isinstance(entry, dict):
                return {}
            for key, value in entry.items():
                values.setdefault(key, []).append(value)
    for key, value in matrix.items():
        if key in {"include", "exclude"} or not isinstance(value, list):
            continue
        values.setdefault(key, []).extend(value)
    return values


def _job_display_names(job: dict) -> list[str]:
    """Every display name a job produces, with unknown values left as a token."""
    name = job.get("name")
    if not isinstance(name, str) or not name.strip():
        return []
    names = {name}
    for key, options in _matrix_values(job).items():
        expression = re.compile(r"\$\{\{\s*matrix\." + re.escape(key) + r"\s*\}\}")
        expanded = set()
        for candidate in names:
            if expression.search(candidate):
                expanded.update(expression.sub(str(option), candidate) for option in options)
            else:
                expanded.add(candidate)
        names = expanded
    return sorted({_display_shape(candidate) for candidate in names})

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


def _workflow_triggers(data: dict) -> set[str] | None:
    triggers = data.get("on", data.get(True, {}))
    if isinstance(triggers, str):
        triggers = {triggers: None}
    if isinstance(triggers, list) and all(isinstance(item, str) for item in triggers):
        triggers = {item: None for item in triggers}
    if not isinstance(triggers, dict) or not triggers or not all(isinstance(item, str) for item in triggers):
        return None
    return set(triggers)


def check_registry_names(workflows=None) -> list[Violation]:
    """Hold the registry's own display names to the naming rules."""
    import ci_contract

    path = "scripts/ci_contract.py"
    violations = []
    for workflow in ci_contract.WORKFLOWS if workflows is None else workflows:
        if workflow.category not in ci_contract.CATEGORIES:
            violations.append(Violation("ci-workflow-name", path, 0,
                f"'{workflow.name}' uses category '{workflow.category}'; "
                f"the categories are {', '.join(ci_contract.CATEGORIES)}"))
        for retired in ci_contract.RETIRED_CATEGORIES:
            if re.search(rf"(?:^|[ /]){re.escape(retired)}(?:[ /]|$)", workflow.name):
                violations.append(Violation("ci-workflow-name", path, 0,
                    f"'{workflow.name}' uses the retired category '{retired}'"))
        subjects = [(workflow.name, f"workflow '{workflow.name}'")]
        subjects += [(job.name, f"job '{job.name}' in '{workflow.name}'") for job in workflow.jobs]
        for text, subject in subjects:
            for problem in ci_contract.title_case_violations(text):
                violations.append(Violation("ci-display-name", path, 0, f"{subject}: {problem}"))
            if TRIGGER_WORDS_RE.search(VARIANT_RE.sub("", text)):
                violations.append(Violation("ci-display-name", path, 0,
                    f"{subject} names a trigger; a display name identifies responsibility"))
            if BARE_ORDINAL_RE.search(text):
                violations.append(Violation("ci-display-name", path, 0,
                    f"{subject} carries a bare ordinal; label the replica or shard"))
    return violations


def check_workflow_rules(repo_root: Path, files: list[str]) -> list[Violation]:
    """Validate the complete tracked-file inventory, not a changed-file subset."""
    import ci_contract

    violations = check_registry_names()
    tracked = {path for path in files
               if path.startswith(".github/workflows/") and path.endswith((".yml", ".yaml"))}
    registered = set(ci_contract.registered_paths())
    for path in sorted(registered - tracked):
        violations.append(Violation("ci-workflow-registration", path, 0,
            "scripts/ci_contract.py registers this workflow but no file is tracked at that path"))
    for path in sorted(tracked - registered):
        violations.append(Violation("ci-workflow-registration", path, 0,
            "workflow is not registered in scripts/ci_contract.py"))
    for rel_path in sorted(tracked & registered):
        if (repo_root / rel_path).is_file():
            violations.extend(check_workflow_file(repo_root, rel_path, ci_contract.by_path(rel_path)))
    violations.extend(check_nes_compositions(repo_root, tracked))
    violations.extend(check_nes_case_coverage(repo_root, tracked))
    violations.extend(check_miri_matrices(repo_root, tracked))
    return violations


def check_workflow_file(repo_root: Path, rel_path: str, workflow) -> list[Violation]:
    """Hold one workflow file to the name, triggers and jobs it is registered with."""
    violations = []
    try:
        data = _parse_workflow(repo_root / rel_path)
    except (OSError, ValueError) as error:
        return [Violation("ci-workflow-parse", rel_path, 0, str(error))]
    name = str(data.get("name", "")).strip()
    if name != workflow.name:
        violations.append(Violation("ci-workflow-name", rel_path, 0,
            f"workflow is named '{name}' and registered as '{workflow.name}'"))
    triggers = _workflow_triggers(data)
    if triggers is None:
        violations.append(Violation("ci-workflow-parse", rel_path, 0, "workflow has no valid triggers"))
        return violations
    if triggers != set(workflow.triggers):
        violations.append(Violation("ci-workflow-triggers", rel_path, 0,
            f"triggers {sorted(triggers)} do not match the registered {sorted(workflow.triggers)}"))
    violations.extend(check_workflow_jobs(rel_path, workflow, data, bool(triggers & PR_EVENTS)))
    return violations


def check_workflow_jobs(rel_path: str, workflow, data: dict, pr_triggered: bool) -> list[Violation]:
    """Match every job to the responsibility the registry gives it."""
    patterns = [(job, _variant_pattern(job.name)) for job in workflow.jobs]
    violations = []
    claimed: set[str] = set()
    for job_id, job in data["jobs"].items():
        displays = _job_display_names(job)
        if not displays:
            violations.append(Violation("ci-display-name", rel_path, 0,
                f"job '{job_id}' has no display name"))
            continue
        registered = None
        for display in displays:
            candidates = [entry for entry in patterns if entry[1].fullmatch(display)]
            if not candidates:
                violations.append(Violation("ci-display-name", rel_path, 0,
                    f"job '{job_id}' displays '{display}', which no job registered "
                    f"under '{workflow.name}' owns"))
                continue
            candidates.sort(key=lambda entry: (len(VARIANT_RE.findall(entry[0].name)),
                                               -len(VARIANT_RE.sub("", entry[0].name))))
            registered = candidates[0][0]
            claimed.add(registered.name)
        if registered is not None:
            violations.extend(check_job_contract(rel_path, job_id, job, registered, pr_triggered))
    for job in workflow.jobs:
        if job.name not in claimed:
            violations.append(Violation("ci-display-name", rel_path, 0,
                f"no job displays the registered '{job.name}'"))
    return violations


def check_job_contract(rel_path: str, job_id: str, job: dict, registered, pr_triggered: bool) -> list[Violation]:
    """Enforce the routing and budget the registry records for one job."""
    violations = []
    timeout = job.get("timeout-minutes")
    declared = isinstance(timeout, int) and not isinstance(timeout, bool)
    runs_on_pr = pr_triggered and not _job_skips_pr(str(job.get("if", "")))
    if registered.trigger == "pr":
        if pr_triggered and not runs_on_pr:
            violations.append(Violation("ci-trigger-routing", rel_path, 0,
                f"job '{job_id}' is a registered pull request check but its guard excludes pull requests"))
        if not declared or not 0 < timeout <= PR_JOB_MAX_TIMEOUT_MINUTES:
            violations.append(Violation("ci-pr-job-timeout", rel_path, 0,
                f"job '{job_id}' has timeout-minutes={timeout} and must set "
                f"<= {PR_JOB_MAX_TIMEOUT_MINUTES}"))
    else:
        if runs_on_pr:
            violations.append(Violation("ci-trigger-routing", rel_path, 0,
                f"job '{job_id}' reaches pull requests but is registered as schedule or dispatch work"))
        if pr_triggered and not registered.exception:
            violations.append(Violation("ci-trigger-exception", rel_path, 0,
                f"job '{job_id}' mixes trigger classes without a registered exception"))
        if timeout is None:
            violations.append(Violation("ci-pr-job-timeout", rel_path, 0,
                f"job '{job_id}' declares no timeout-minutes"))
    if declared and timeout != registered.timeout_minutes:
        violations.append(Violation("ci-pr-job-timeout", rel_path, 0,
            f"job '{job_id}' has timeout-minutes={timeout}; the registry records "
            f"{registered.timeout_minutes}"))
    violations.extend(check_job_scope(rel_path, job_id, job, registered))
    return violations


def check_job_scope(rel_path: str, job_id: str, job: dict, registered) -> list[Violation]:
    """Change selection belongs inside the job that owns the work."""
    def problem(message):
        return [Violation("ci-scope-routing", rel_path, 0, f"job '{job_id}' {message}")]

    steps = [step for step in job.get("steps", []) if isinstance(step, dict)]
    selectors = [step for step in steps
                 if str(step.get("uses", "")).rstrip("/").endswith(".github/actions/ci-scope")]
    if not registered.scope:
        if selectors:
            return problem("selects work under a kind the registry does not record")
        return []
    if len(selectors) != 1:
        return problem(f"must select '{registered.scope}' exactly once")
    selector = selectors[0]
    kind = (selector.get("with") or {}).get("kind")
    if kind != registered.scope:
        return problem(f"selects '{kind}' and is registered for '{registered.scope}'")
    if selector.get("id") != "scope" or "if" in selector or selector.get("continue-on-error", False):
        return problem("must run the selector unconditionally under the id 'scope'")
    checkout = next((step for step in steps if "actions/checkout" in str(step.get("uses", ""))), None)
    if checkout is None or (checkout.get("with") or {}).get("fetch-depth") != 0:
        return problem("must check out the complete diff the selector reads")
    guarded = False
    for step in steps:
        condition = _strip_outer_parentheses(
            str(step.get("if", "")).replace("${{", "").replace("}}", "").strip())
        if "steps.scope.outputs.enabled" not in condition:
            continue
        if len(_split_condition(condition, "||")) > 1:
            return problem("must require its selection with &&, never behind an || fallback")
        guarded = True
    if not guarded:
        return problem("must guard its selected work with steps.scope.outputs.enabled")
    return []


def check_nes_compositions(repo_root: Path, tracked: set[str]) -> list[Violation]:
    """Both NES compositions keep a bounded check and a full benchmark."""
    import ci_contract

    violations = []
    for composition in NES_COMPOSITIONS_REQUIRED:
        if composition not in ci_contract.NES_COMPOSITIONS:
            violations.append(Violation("ci-nes-compositions", "scripts/ci_contract.py", 0,
                f"the {composition} NES composition is not registered"))
    for composition, entry in ci_contract.NES_COMPOSITIONS.items():
        for role in ("checks", "benchmarks"):
            workflow = ci_contract.by_name(entry[role])
            if workflow.path not in tracked or not (repo_root / workflow.path).is_file():
                violations.append(Violation("ci-nes-compositions", workflow.path, 0,
                    f"the {composition} NES composition needs its {role} workflow"))
    return violations


def check_nes_case_coverage(repo_root: Path, tracked: set[str]) -> list[Violation]:
    """Require the public roster to map one-to-one to independently runnable jobs."""
    import ci_contract

    composition = ci_contract.NES_COMPOSITIONS.get("Dissonance Workloads")
    if composition is None:
        return []
    workflow = ci_contract.by_name(composition["benchmarks"])
    manifest = "benchmarks/search/nightly.json"

    def problem(message):
        return [Violation("ci-nes-case-jobs", workflow.path, 0, message)]

    if manifest not in tracked and not (repo_root / manifest).is_file():
        return []
    if workflow.path not in tracked or not (repo_root / workflow.path).is_file():
        return problem("the registered public NES manifest requires its tracked owning workflow")
    try:
        expected = [case["id"] for case in json.loads((repo_root / manifest).read_text())["cases"]]
    except (OSError, ValueError, KeyError, TypeError) as error:
        return problem(f"cannot read public NES case manifest: {error}")
    if not expected or len(expected) != len(set(expected)):
        return problem("public NES manifest must contain unique, nonempty cases")
    try:
        data = _parse_workflow(repo_root / workflow.path)
    except (OSError, ValueError) as error:
        return [Violation("ci-workflow-parse", workflow.path, 0, str(error))]
    actual = []
    campaign_jobs = []
    for job_id, job in data["jobs"].items():
        commands = "\n".join(str(step.get("run", "")) for step in job.get("steps", [])
                             if isinstance(step, dict))
        if "eval.py run" not in commands:
            continue
        campaign_jobs.append(job_id)
        strategy = job.get("strategy", {})
        matrix = strategy.get("matrix") if isinstance(strategy, dict) else None
        include = matrix.get("include") if isinstance(matrix, dict) else None
        if (not isinstance(include, list) or set(matrix) != {"include"}
                or not all(isinstance(entry, dict) and isinstance(entry.get("case"), str)
                           and isinstance(entry.get("name"), str) for entry in include)
                or strategy.get("fail-fast") is not False
                or not re.search(r"--case\s+[\"']?\$\{\{\s*matrix\.case\s*\}\}", commands)
                or manifest not in commands):
            return problem(f"job '{job_id}' must run {manifest} with --case matrix.case, "
                           "a static include matrix naming every scenario, and fail-fast: false")
        actual.extend(entry["case"] for entry in include)
    if sorted(actual) != sorted(expected):
        return problem("NES case matrices must cover every public manifest case exactly once")
    report = next((job for job in data["jobs"].values() if job.get("name") == "Results"), None)
    if report is None:
        return problem("the benchmark workflow must publish a Results job")
    needs = report.get("needs", [])
    if isinstance(needs, str):
        needs = [needs]
    if (str(report.get("if", "")).replace("${{", "").replace("}}", "").strip() != "always()"
            or not set(campaign_jobs).issubset(needs)):
        return problem("Results must use always() and depend on every campaign matrix")
    return []


def _miri_matrix_names(data: dict, suffix: str) -> list[str]:
    wanted = f"Miri — ${{{{ matrix.name }}}}{suffix}"
    for job in data["jobs"].values():
        if job.get("name") != wanted:
            continue
        matrix = (job.get("strategy") or {}).get("matrix")
        if not isinstance(matrix, dict):
            return []
        include = matrix.get("include")
        if isinstance(include, list):
            return [entry.get("name") for entry in include if isinstance(entry, dict)]
        names = matrix.get("name")
        return list(names) if isinstance(names, list) else []
    return []


def check_miri_matrices(repo_root: Path, tracked: set[str]) -> list[Violation]:
    """Each component's Analysis workflow lists exactly the targets it owns."""
    import ci_contract
    from miri_scope import TARGETS, whole_crate_targets

    violations = []
    bounded: dict[str, list[str]] = {}
    whole: dict[str, list[str]] = {}
    for target in TARGETS:
        owner = ci_contract.MIRI_OWNERS.get(target["name"])
        if owner is None:
            violations.append(Violation("ci-miri-coverage", "scripts/miri_scope.py", 0,
                f"Miri target '{target['name']}' has no registered owning component"))
            continue
        bounded.setdefault(owner, []).append(target["name"])
    for target in whole_crate_targets():
        owner = ci_contract.MIRI_OWNERS.get(target["name"])
        if owner is not None:
            whole.setdefault(owner, []).append(target["name"])
    for owner, workflow_name in ci_contract.MIRI_ANALYSIS_WORKFLOWS.items():
        workflow = ci_contract.by_name(workflow_name)
        if workflow.path not in tracked or not (repo_root / workflow.path).is_file():
            continue
        try:
            data = _parse_workflow(repo_root / workflow.path)
        except (OSError, ValueError) as error:
            violations.append(Violation("ci-workflow-parse", workflow.path, 0, str(error)))
            continue
        for expected, suffix in ((bounded.get(owner, []), ""), (whole.get(owner, []), " (Whole Crate)")):
            listed = _miri_matrix_names(data, suffix)
            if sorted(listed) != sorted(expected):
                violations.append(Violation("ci-miri-coverage", workflow.path, 0,
                    f"the Miri{suffix} matrix lists {sorted(listed)} and "
                    f"scripts/miri_scope.py registers {sorted(expected)} for {owner}"))
    return violations


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
        if rel_path in (str(BASELINE_PATH), "docs/semantic-lints-baseline.json"):
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


# The prohibited term is assembled so this checker follows its own rule.
PROHIBITED_WORD = "ga" + "te"
PROHIBITED_WORD_RE = re.compile(
    rf"(?<![A-Za-z0-9])(?i:{PROHIBITED_WORD}s?)(?![A-Za-z0-9])"
    rf"|(?<![A-Za-z0-9]){PROHIBITED_WORD}s?(?=[A-Z])"
    rf"|{PROHIBITED_WORD.capitalize()}s?(?=[A-Z]|[^A-Za-z0-9]|$)"
)
VOCABULARY_RULE = "no-prohibited-word"


def check_repository_vocabulary(repo_root: Path, files: list[str]) -> list[Violation]:
    """Check every tracked text file and path, including this checker."""
    violations = []
    for rel_path in files:
        if PROHIBITED_WORD_RE.search(rel_path):
            violations.append(Violation(VOCABULARY_RULE, rel_path, 0,
                                        "prohibited word in file path"))
        try:
            data = (repo_root / rel_path).read_bytes()
            if b"\0" in data:
                continue
            text = data.decode("utf-8")
        except (OSError, UnicodeError):
            continue
        for line, content in enumerate(text.splitlines(), 1):
            if PROHIBITED_WORD_RE.search(content):
                violations.append(Violation(VOCABULARY_RULE, rel_path, line, content.strip()))
    return violations


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

    vocabulary_violations = check_repository_vocabulary(root, files + ["scripts/custom-lints.py"])

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
    all_file_violations = vocabulary_violations + comment_violations + misplaced_violations + numbered_violations + workflow_violations + github_dir_violations + golden_violations + docs_violations + toplevel_violations
    new_file_violations: list[Violation] = []
    for v in all_file_violations:
        key = _violation_key(v)
        rule_baseline = set(baseline.get(v.rule, []))
        if key in rule_baseline and not v.rule.startswith("ci-") and v.rule != VOCABULARY_RULE:
            still_baselined.add((v.rule, key))
        else:
            new_file_violations.append(v)

    errors: list[str] = []

    if args.update_baseline:
        ci_errors = [v for v in workflow_violations if v.rule.startswith("ci-")]
        if vocabulary_violations:
            print("cannot baseline prohibited vocabulary; rename every occurrence first", file=sys.stderr)
            return 1
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
        VOCABULARY_RULE: "Use a precise term such as check, guard, requirement, or readiness instead.",
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
            f"Every job a pull request reaches must finish inside "
            f"{PR_JOB_MAX_TIMEOUT_MINUTES} minutes, and every job's declared bound must "
            "match the budget scripts/ci_contract.py records for it. Split the work, or "
            "register the long run as a schedule or dispatch job with its reason."
        ),
        "ci-workflow-parse": "Install PyYAML==6.0.3 and fix malformed or duplicate YAML fields; workflow checks never silently skip parsing.",
        "ci-workflow-registration": "Every workflow file is registered in scripts/ci_contract.py with its component owner, jobs, triggers and budgets. Add the entry, or delete the file.",
        "ci-workflow-name": (
            "A workflow name is 'Category / Component' or 'Category / Composition / Workload', "
            "in Title Case, using the categories Checks, Benchmarks and Release. The file name "
            "must match its registered name exactly."
        ),
        "ci-workflow-triggers": "A workflow's triggers must match the ones scripts/ci_contract.py registers for it.",
        "ci-trigger-routing": "A job registered for pull requests must run on them, and a job registered as schedule or dispatch work must prove it excludes them with an event guard.",
        "ci-trigger-exception": "A workflow reachable from a pull request may hold schedule-only jobs only through an exception registered in scripts/ci_contract.py, which states why the work cannot fit the bounded budget.",
        "ci-display-name": (
            "Display names are Title Case, identify a responsibility or a workload scenario, "
            "and carry variants as ' — <Variant>' with an explicit replica or shard label. "
            "Testing methods belong in step names, triggers belong in nothing. Every job in a "
            "file must be registered in scripts/ci_contract.py and every registered job must exist."
        ),
        "ci-scope-routing": "Select work inside the job that owns it: one ./.github/actions/ci-scope step under the registered kind, a complete diff checkout, and selected steps guarded with && on the selector's output.",
        "ci-nes-case-jobs": "Map every public NES manifest case exactly once to the case matrix, select it with --case, disable fail-fast, and retain an always-running Results job.",
        "ci-nes-compositions": "Both NES compositions stay: Dissonance runs the game on native QuickNES and Harmony runs it inside a Consonance VM. Each keeps a bounded check and a full benchmark.",
        "ci-miri-coverage": "The Analysis workflow of each component must list exactly the Miri targets scripts/miri_scope.py registers and scripts/ci_contract.py assigns to it.",
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
