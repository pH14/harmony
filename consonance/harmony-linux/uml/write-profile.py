#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Write profile.json for a built User-mode Linux artifact directory."""

import argparse
import hashlib
import json
from pathlib import Path

ARTIFACTS = {"executable": "linux", "config": "config", "rootfs": "initramfs.cpio.gz"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def config_int(config: Path, symbol: str) -> int:
    prefix = f"CONFIG_{symbol}="
    for line in config.read_text().splitlines():
        if line.startswith(prefix):
            return int(line[len(prefix) :])
    raise SystemExit(f"{config}: {prefix} missing")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--kernel-version", required=True)
    parser.add_argument("--patch-series", type=Path, required=True)
    args = parser.parse_args()

    profile = {
        "schema": 2,
        "architecture": args.architecture,
        "kernel_version": args.kernel_version,
        "userspace": "seccomp",
        "patch_series_sha256": sha256(args.patch_series),
        "host_libraries": [],
        "virtual_time": {
            "syscall_vns": config_int(args.output / "config", "HARMONY_UML_SYSCALL_VNS"),
            "clock_read_vns": config_int(args.output / "config", "HARMONY_UML_CLOCK_READ_VNS"),
        },
    }
    for role, name in ARTIFACTS.items():
        profile[role] = {"name": name, "sha256": sha256(args.output / name)}
    text = json.dumps(profile, indent=2, sort_keys=True) + "\n"
    (args.output / "profile.json").write_text(text)


if __name__ == "__main__":
    main()
