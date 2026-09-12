#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Select formal safety checks from a list of changed repository paths."""

from __future__ import annotations

import fnmatch
import sys
from collections.abc import Iterable


KANI_PATH_PATTERNS = (
    "consonance/vtime/**",
    "consonance/lapic/**",
    "consonance/vmm-backend/**",
    "workloads/fault-policy/**",
    "Cargo.toml",
    "Cargo.lock",
    "**/Cargo.toml",
    "**/Cargo.lock",
    "rust-toolchain.toml",
    "flake.nix",
    "flake.lock",
    "clippy.toml",
    "deny.toml",
    ".cargo/**",
    ".github/workflows/quality.yml",
    ".github/workflows/nightly.yml",
)


def kani_required(paths: Iterable[str]) -> bool:
    """Return whether any changed path can affect a Kani proof or its inputs."""
    return any(
        fnmatch.fnmatchcase(path, pattern)
        for path in paths
        for pattern in KANI_PATH_PATTERNS
    )


def main() -> int:
    print("true" if kani_required(line.strip() for line in sys.stdin) else "false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
