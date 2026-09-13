#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the bounded per-crate Miri matrix for a pull request."""

from __future__ import annotations

import fnmatch
import json
import sys
from collections.abc import Iterable


TARGETS = (
    {
        "name": "vmm-core",
        "command": "-p vmm-core --lib vendor::x86::bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping",
        "paths": ("consonance/vmm-core/**",),
    },
    {
        "name": "hypercall-doorbell",
        "command": "-p hypercall-doorbell --features linux-device",
        "paths": ("consonance/hypercall-doorbell/**",),
    },
    {
        "name": "consonance-client",
        "command": "-p consonance-client",
        "paths": ("consonance/client/**",),
    },
    {"name": "vm-state", "command": "-p vm-state", "paths": ("consonance/vm-state/**",)},
    {
        "name": "vmm-backend",
        "command": "-p vmm-backend --all-features",
        "paths": ("consonance/vmm-backend/**",),
    },
    {
        "name": "snapshot-store",
        "command": "-p snapshot-store --lib",
        "miriflags": "-Zmiri-permissive-provenance -Zmiri-disable-isolation",
        "paths": ("consonance/snapshot-store/**",),
    },
    {
        "name": "machine",
        "command": "--manifest-path workloads/nes-machine/Cargo.toml",
        "paths": ("workloads/nes-machine/**",),
    },
    {
        "name": "nes-guest",
        "command": "--manifest-path workloads/nes-guest/Cargo.toml --lib --test agent --bins",
        "paths": ("workloads/nes-guest/**",),
    },
    {
        "name": "harmony-supervisor",
        "command": "--manifest-path consonance/harmony-linux/supervisor/Cargo.toml --lib --bins",
        "paths": ("consonance/harmony-linux/supervisor/**",),
    },
)

GLOBAL_PATH_PATTERNS = (
    "Cargo.toml",
    "Cargo.lock",
    "**/Cargo.toml",
    "**/Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    "flake.nix",
    "flake.lock",
    ".cargo/**",
    "clippy.toml",
    "deny.toml",
    ".github/workflows/nightly.yml",
)


def _matches(paths: Iterable[str], patterns: Iterable[str]) -> bool:
    return any(
        fnmatch.fnmatchcase(path, pattern)
        for path in paths
        for pattern in patterns
    )


def selected_targets(paths: Iterable[str]) -> list[dict[str, str]]:
    normalized = [path.strip() for path in paths if path.strip()]
    select_all = _matches(normalized, GLOBAL_PATH_PATTERNS)
    selected: list[dict[str, str]] = []
    for target in TARGETS:
        if select_all or _matches(normalized, target["paths"]):
            entry = {key: value for key, value in target.items() if key != "paths"}
            entry.setdefault("miriflags", "-Zmiri-permissive-provenance")
            selected.append(entry)
    return selected


def main() -> int:
    print(json.dumps({"include": selected_targets(sys.stdin)}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
