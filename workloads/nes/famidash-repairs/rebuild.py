# SPDX-License-Identifier: AGPL-3.0-or-later
"""Rebuild the exact Harmony ROMs from a pristine pinned Famidash checkout."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def rebuild(source, compiler):
    manifest = json.loads((ROOT / "source-files.json").read_text())
    roms = json.loads((ROOT / "roms.json").read_text())
    revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if revision != manifest["source_commit"]:
        raise ValueError("Famidash source revision mismatch")
    compiler_revision = subprocess.check_output(["git", "-C", str(compiler), "rev-parse", "HEAD"], text=True).strip()
    if compiler_revision != "2f25b1807450db2d386f027b6d81dc906ab03396":
        raise ValueError("cc65 source revision mismatch")
    if subprocess.check_output(["git", "-C", str(compiler), "status", "--porcelain", "--untracked-files=no"], text=True):
        raise ValueError("cc65 checkout must be unmodified")
    if subprocess.check_output(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=no"], text=True):
        raise ValueError("Famidash checkout must be unmodified")
    patch = ROOT / "source.patch"
    if digest(patch) != manifest["patch_sha256"]:
        raise ValueError("source patch checksum mismatch")
    for entry in manifest["changes"]:
        path = source / entry["path"]
        before = digest(path) if path.exists() else None
        if before != entry["before_sha256"]:
            raise ValueError(f"source file mismatch: {entry['path']}")
    subprocess.run(["git", "-C", str(source), "apply", "--check", str(patch)], check=True)
    subprocess.run(["git", "-C", str(source), "apply", str(patch)], check=True)
    for entry in manifest["changes"]:
        if digest(source / entry["path"]) != entry["after_sha256"]:
            raise ValueError(f"patched source file mismatch: {entry['path']}")
    subprocess.run(["make", "-B", "-j2", "main", "b-sides", "c-sides", "d-sides",
                    *[f"{tool.upper()}={compiler / 'bin' / tool}" for tool in ["cc65", "ca65", "ld65"]]],
                   cwd=source, check=True)
    for build, entry in zip(["main", "b-sides", "c-sides", "d-sides"], roms["files"]):
        if digest(source / "BUILD" / build / "famidash.nes") != entry["output_sha256"]:
            raise ValueError(f"reproduced ROM mismatch: {build}")
    print("All four rebuilt ROMs match the shipped repair hashes.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("cc65", type=Path)
    args = parser.parse_args()
    rebuild(args.source.resolve(), args.cc65.resolve())
