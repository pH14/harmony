#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fail when a test exists that nothing registered runs.

`ci_contract.LIB_PARTITIONS` divides one package's unit tests between the jobs
that own its responsibilities. A new module lands with no owner unless something
notices, so the owning workflow runs this after its own share.

`ignored` lists every ignored test this host builds and requires each one to
match a job's `ignored_tests` or `ci_contract.HOST_TESTS`. Which ignored tests
build depends on the host, so CI runs it on every host it has.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from ci_contract import (
    CARGO_MANIFESTS, LIB_PARTITIONS, ROOT, nextest_orphan_filter, runners_of, this_host,
)


LIST_ARGUMENTS = {
    "vmm-core": ("-p", "vmm-core", "--lib", "--all-features"),
    "searcher": (
        "--manifest-path", "dissonance/Cargo.toml", "-p", "searcher",
        "--lib", "--all-features",
    ),
}


def orphans(package: str) -> list[str]:
    command = [
        "cargo", "nextest", "list", "--message-format", "json",
        "-E", nextest_orphan_filter(package), *LIST_ARGUMENTS[package],
    ]
    listing = json.loads(subprocess.run(command, check=True, text=True,
                                        stdout=subprocess.PIPE).stdout)
    return sorted(
        name
        for suite in listing.get("rust-suites", {}).values()
        for name, case in suite.get("testcases", {}).items()
        if case.get("filter-match", {}).get("status") == "matches"
    )


def _run_json(command: list[str]) -> dict:
    return json.loads(subprocess.run(command, check=True, text=True, cwd=ROOT,
                                     stdout=subprocess.PIPE).stdout)


def manifests_with_ignored_tests() -> list[str]:
    """The manifests whose packages hold an ignored test, found without a build."""
    sources = subprocess.run(["git", "ls-files", "*.rs"], check=True, text=True, cwd=ROOT,
                             stdout=subprocess.PIPE).stdout.split()
    marked = [path for path in sources if "#[ignore" in (ROOT / path).read_text(errors="replace")]
    found = []
    for manifest in CARGO_MANIFESTS:
        metadata = _run_json(["cargo", "metadata", "--no-deps", "--format-version", "1",
                              "--manifest-path", manifest])
        roots = [Path(package["manifest_path"]).parent.relative_to(ROOT).as_posix() + "/"
                 for package in metadata["packages"]]
        if any(path.startswith(root) for path in marked for root in roots):
            found.append(manifest)
    return found


def ignored_tests(manifest: str) -> list[tuple[str, str]]:
    listing = _run_json([
        "cargo", "nextest", "list", "--message-format", "json", "--manifest-path", manifest,
        "--workspace", "--all-features", "--run-ignored", "only",
    ])
    return sorted(
        (binary_id, name)
        for binary_id, suite in listing.get("rust-suites", {}).items()
        for name, case in suite.get("testcases", {}).items()
        if case.get("ignored")
    )


def _runner_kind(runner: str) -> str:
    if runner.startswith("pre-push"):
        return "in the pre-push hook"
    return "in CI"


def check_ignored() -> int:
    counts: dict[str, int] = {}
    unrun = []
    for manifest in manifests_with_ignored_tests():
        for binary_id, name in ignored_tests(manifest):
            runners = runners_of(binary_id, name)
            if not runners:
                unrun.append(f"{binary_id} {name}")
            for kind in {_runner_kind(runner) for runner in runners}:
                counts[kind] = counts.get(kind, 0) + 1
    if unrun:
        listing = "\n  ".join(unrun)
        raise SystemExit(
            f"{this_host()}: nothing registered runs these ignored tests. Add each to "
            f"the ignored_tests of the job that runs it, or to HOST_TESTS for a machine "
            f"that can, in scripts/ci_contract.py, or delete it:\n  {listing}"
        )
    summary = ", ".join(f"{count} {kind}" for kind, count in sorted(counts.items()))
    print(f"{this_host()}: every ignored test has a runner ({summary})")
    return 0


def main(argv: list[str]) -> int:
    modes = (*LIB_PARTITIONS, "ignored")
    if len(argv) != 1 or argv[0] not in modes:
        raise SystemExit(f"usage: check-test-partition.py {{{'|'.join(modes)}}}")
    if argv[0] == "ignored":
        return check_ignored()
    package = argv[0]
    unowned = orphans(package)
    if unowned:
        listing = "\n  ".join(unowned)
        raise SystemExit(
            f"{package}: no job in the CI contract runs these tests. Add their "
            f"module to a job in ci_contract.LIB_PARTITIONS[{package!r}]:\n  {listing}"
        )
    owners = ", ".join(sorted(LIB_PARTITIONS[package]))
    print(f"{package}: every test belongs to one of {owners}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
