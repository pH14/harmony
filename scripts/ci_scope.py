#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Select bounded workload checks and API checks from changed repository paths.

Each name is one registered job's change-selection kind; `ci_contract.SCOPE_KINDS`
maps it to the workflow that owns it.
"""

import fnmatch
import sys


SCENARIOS = (
    "dissonance_nes",
    "dissonance_stb",
    "harmony_nes",
    "harmony_oci",
    "consonance_platform",
    "consonance_kvm",
)
GLOBAL = (
    "Cargo.toml", "Cargo.lock", "**/Cargo.toml", "**/Cargo.lock",
    "rust-toolchain*", "flake.nix", "flake.lock", ".cargo/**",
    ".github/workflows/dissonance-workloads-nes-checks.yml",
    ".github/workflows/harmony-workloads-nes-checks.yml",
    ".github/workflows/harmony-workloads-oci-checks.yml",
    ".github/workflows/consonance-checks.yml",
    ".github/actions/platform-runtime/**",
    "scripts/ci_scope.py", "scripts/test_ci_scope.py",
    "scripts/ci_contract.py", "scripts/test_ci_contract.py",
    ".github/actions/ci-scope/**", "scripts/ci-job-scope.py", "scripts/test_ci_job_scope.py",
)


def selected(paths):
    result = dict.fromkeys((*SCENARIOS, "public_api"), False)
    for path in paths:
        path = path.strip()
        if not path or path.endswith(".md"):
            continue
        if any(fnmatch.fnmatchcase(path, pattern) for pattern in GLOBAL):
            return dict.fromkeys(result, True)
        if path.startswith(("dissonance/", "cli/")):
            result["dissonance_nes"] = result["harmony_nes"] = True
        if path.startswith(("workloads/nes/", "workloads/nes-machine/", "workloads/nes-protocol/", "workloads/nes-guest/")):
            if path.startswith(("workloads/nes/src/stb/", "workloads/nes/src/bin/stb-")) or "/stb-" in path or path.endswith("build-stb-rom.sh"):
                result["dissonance_stb"] = True
            else:
                result["dissonance_nes"] = result["harmony_nes"] = True
        if path == "scripts/build-quicknes-core.sh":
            result["dissonance_nes"] = result["dissonance_stb"] = result["harmony_nes"] = True
        if path.startswith(".github/actions/stb-evaluation/"):
            result["dissonance_stb"] = True
        if path.startswith(("workloads/fault-policy/", "workloads/faults/", "workloads/bugs/historical/postgres-cic-corruption/", "scripts/historical-")):
            result["harmony_oci"] = True
        if path in (".github/workflows/harmony-workloads-historical-bugs.yml", "scripts/render-historical-bugs.py"):
            result["harmony_oci"] = True
        if path.startswith("cli/"):
            result["harmony_oci"] = result["consonance_platform"] = True
        if path.startswith("consonance/"):
            result["consonance_platform"] = result["public_api"] = result["harmony_nes"] = True
        if path.startswith(("consonance/vmm-backend/", "consonance/vmm-core/", "consonance/vm-state/", "consonance/vtime/")):
            result["consonance_kvm"] = True
        if path.startswith(("consonance/execution-proto/", "consonance/process-proto/", "consonance/oci/", "consonance/client/")):
            result["harmony_oci"] = True
    return result


if __name__ == "__main__":
    for key, value in selected(sys.stdin).items():
        print(f"{key}={str(value).lower()}")
