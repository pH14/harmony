#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fail when a package split across jobs has tests no job runs.

`ci_contract.LIB_PARTITIONS` divides one package's unit tests between the jobs
that own its responsibilities. A new module lands with no owner unless something
notices, so the owning workflow runs this after its own share.
"""

from __future__ import annotations

import json
import subprocess
import sys

from ci_contract import LIB_PARTITIONS, nextest_orphan_filter


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


def main(argv: list[str]) -> int:
    if len(argv) != 1 or argv[0] not in LIB_PARTITIONS:
        raise SystemExit(f"usage: check-test-partition.py {{{'|'.join(LIB_PARTITIONS)}}}")
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
