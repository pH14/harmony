#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Select a workflow's Rust build and test jobs from changed repository paths.

A change selects the jobs unless every changed path is one that cannot reach
them. Any path this module does not recognize selects the jobs.
"""

from __future__ import annotations

import fnmatch
import sys
from collections.abc import Iterable
from pathlib import Path

from ci_scope import selected as scenarios_selected


WORKFLOW_DIR = ".github/workflows/"

DOCUMENTATION = (
    "*.md", "docs/**", "LICENSE", ".agents/**", ".claude/**", ".githooks/**",
    "benchmarks/**",
)

# Tooling that only the Repository workflow runs. `test_rust_scope.py` fails
# when another workflow or composite action starts naming one of these files.
REPOSITORY_ONLY = (
    "scripts/custom-lints.py", "scripts/test_custom_lints.py",
    "scripts/semantic-lints.py", "scripts/test_semantic_lints.py",
    "scripts/strip-comments.py",
    "scripts/check-dependency-boundaries.py", "scripts/test_check_dependency_boundaries.py",
    "scripts/dependency-boundaries.toml",
    "scripts/fit-vtime-costs.py",
    "scripts/benchmark-report.test.cjs", "scripts/check-portable-tests.test.sh",
)


def _matches(path: str, patterns: Iterable[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def _cannot_reach(path: str, workflow: str) -> bool:
    if any(scenarios_selected([path]).values()):
        return False
    if _matches(path, DOCUMENTATION) or path in REPOSITORY_ONLY:
        return True
    return path.startswith(WORKFLOW_DIR) and Path(path).name != workflow


def rust_checks_required(paths: Iterable[str], workflow: str) -> bool:
    """Return whether any changed path can affect `workflow`'s Rust jobs."""
    return any(not _cannot_reach(path, workflow) for path in (line.strip() for line in paths) if path)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: rust_scope.py WORKFLOW_FILE_NAME < changed-paths", file=sys.stderr)
        return 2
    print("true" if rust_checks_required(sys.stdin, sys.argv[1]) else "false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
