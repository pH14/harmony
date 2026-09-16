#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Select bounded product smokes and API checks from changed repository paths."""

import fnmatch
import sys


SMOKES = ("native", "stb", "faults", "platform", "kvm")
GLOBAL = (
    "Cargo.toml", "Cargo.lock", "**/Cargo.toml", "**/Cargo.lock",
    "rust-toolchain*", "flake.nix", "flake.lock", ".cargo/**",
    ".github/workflows/product-smoke.yml", "scripts/ci_scope.py",
    "scripts/test_ci_scope.py",
)


def selected(paths):
    result = dict.fromkeys((*SMOKES, "public_api"), False)
    for path in paths:
        path = path.strip()
        if not path or path.endswith(".md"):
            continue
        if any(fnmatch.fnmatchcase(path, pattern) for pattern in GLOBAL):
            return dict.fromkeys(result, True)
        if path.startswith(("dissonance/", "cli/")):
            result["native"] = True
        if path.startswith(("workloads/nes/", "workloads/nes-machine/", "workloads/nes-protocol/", "workloads/nes-guest/")):
            if path.startswith(("workloads/nes/src/stb/", "workloads/nes/src/bin/stb-")) or "/stb-" in path or path.endswith("build-stb-rom.sh"):
                result["stb"] = True
            else:
                result["native"] = True
        if path == "scripts/build-quicknes-core.sh":
            result["native"] = result["stb"] = True
        if path.startswith(".github/actions/stb-evaluation/"):
            result["stb"] = True
        if path.startswith(("workloads/fault-policy/", "workloads/faults/", "workloads/bugs/historical/postgres-cic-corruption/", "scripts/historical-")):
            result["faults"] = True
        if path in (".github/workflows/historical-bugs.yml", "scripts/render-historical-bugs.py"):
            result["faults"] = True
        if path.startswith("cli/"):
            result["faults"] = result["platform"] = True
        if path == ".github/workflows/quality.yml":
            result["public_api"] = True
        if path.startswith("consonance/"):
            result["platform"] = result["public_api"] = True
        if path.startswith(("consonance/vmm-backend/", "consonance/vmm-core/", "consonance/vm-state/", "consonance/vtime/")):
            result["kvm"] = True
        if path.startswith(("consonance/execution-proto/", "consonance/process-proto/", "consonance/oci/", "consonance/client/")):
            result["faults"] = True
    return result


if __name__ == "__main__":
    for key, value in selected(sys.stdin).items():
        print(f"{key}={str(value).lower()}")
