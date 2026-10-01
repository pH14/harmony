#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Bind every supported native WASM consumer's dependencies into one identity."""
import argparse
import hashlib
from pathlib import Path

LOCKFILES = (
    "Cargo.lock",
    "consonance/wasm/Cargo.lock",
    "workloads/nes-wasm/Cargo.lock",
    "workloads/nes/Cargo.lock",
)


def dependency_identity(root):
    digest = hashlib.sha256(b"harmony-wasm-consumer-dependencies-v1")
    digest.update(hashlib.sha256(Path(__file__).read_bytes()).digest())
    for name in LOCKFILES:
        path = name.encode()
        digest.update(len(path).to_bytes(8, "little"))
        digest.update(path)
        digest.update(hashlib.sha256((root / name).read_bytes()).digest())
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    identity = dependency_identity(root)
    if args.cargo:
        for name in LOCKFILES:
            print(f"cargo:rerun-if-changed={root / name}")
        print(f"cargo:rustc-env=HARMONY_WASMI_DEPENDENCIES={identity}")
    else:
        print(identity)


if __name__ == "__main__":
    main()
