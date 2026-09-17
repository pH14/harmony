#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Print the cache and artifact key of the ROM-free NES guest image.

The key covers the platform runtime sources and the guest image sources. It
ignores build output so a publisher that has already built the image derives
the same key a consumer derives from a clean checkout.
"""

import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "runtime_artifacts", ROOT / "consonance/harmony-linux/scripts/runtime-artifacts.py")
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)

INPUTS = ("workloads/nes-guest", "scripts/build-quicknes-core.sh")


def guest_digest(repo):
    paths = []
    for name in INPUTS:
        root = repo / name
        if not root.exists():
            raise ValueError(f"missing guest image source: {name}")
        if root.is_file():
            paths.append(root)
            continue
        for directory, directories, files in os.walk(root):
            directories[:] = [entry for entry in directories if entry not in RUNTIME.EXCLUDED]
            paths.extend(Path(directory) / entry for entry in files)
    value = hashlib.sha256()
    for path in sorted(paths):
        relative = path.relative_to(repo)
        if RUNTIME.EXCLUDED.intersection(relative.parts):
            continue
        if path.is_symlink():
            raise ValueError(f"source symlink is unsupported: {relative}")
        value.update(relative.as_posix().encode() + b"\0")
        value.update(RUNTIME.digest(path).encode() + b"\n")
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=ROOT)
    args = parser.parse_args()
    print(f"{RUNTIME.source_digest(args.repo)}-{guest_digest(args.repo)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
