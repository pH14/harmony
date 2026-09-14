#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import sys

INPUTS = (
    "Cargo.toml", "Cargo.lock", "flake.nix", "flake.lock",
    "consonance/harmony-linux", "consonance/hypercall-proto",
    "consonance/hypercall-doorbell", "consonance/execution-proto",
    "consonance/process-proto",
)
EXCLUDED = {".git", "target", "build", "dl", "__pycache__"}
MANIFEST = "runtime-manifest.json"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def source_digest(repo):
    paths = []
    for name in INPUTS:
        root = repo / name
        if not root.exists():
            raise ValueError(f"missing platform source: {name}")
        candidates = []
        if root.is_dir():
            for directory, directories, files in os.walk(root):
                directories[:] = [name for name in directories if name not in EXCLUDED]
                for name in directories + files:
                    path = Path(directory) / name
                    if path.is_symlink():
                        raise ValueError(f"source symlink is unsupported: {path}")
                candidates.extend(Path(directory) / name for name in files)
        else:
            candidates.append(root)
        for path in candidates:
            relative = path.relative_to(repo)
            if EXCLUDED.intersection(relative.parts):
                continue
            if path.is_symlink():
                raise ValueError(f"source symlink is unsupported: {relative}")
            if path.is_file():
                paths.append(path)
    value = hashlib.sha256()
    for path in sorted(paths):
        value.update(path.relative_to(repo).as_posix().encode() + b"\0")
        value.update(digest(path).encode() + b"\n")
    return value.hexdigest()


def artifacts(root, architecture):
    kernel = "bzImage" if architecture == "x86_64" else "Image"
    paths = [root / kernel, root / "initramfs-oci.cpio.gz"]
    fixture = root / "fixture"
    paths.extend(path for path in fixture.rglob("*") if path.is_file())
    for required in (fixture / "index.json", fixture / "oci-layout"):
        if required not in paths:
            raise ValueError(f"missing fixture metadata: {required}")
    if not (fixture / "blobs" / "sha256").is_dir():
        raise ValueError("missing fixture blobs")
    result = {}
    for path in sorted(paths):
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"missing or unsupported artifact: {path}")
        result[path.relative_to(root).as_posix()] = digest(path)
    return result


def seal(repo, root, architecture):
    manifest = {
        "version": 1,
        "architecture": architecture,
        "source_digest": source_digest(repo),
        "files": artifacts(root, architecture),
    }
    (root / MANIFEST).write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n")
    return manifest


def verify(repo, root, architecture):
    manifest_path = root / MANIFEST
    if not root.is_dir():
        return {"scope": "unavailable", "reason": "artifact directory is missing"}
    if not manifest_path.is_file():
        return {"scope": "inconclusive", "reason": "artifact manifest is missing"}
    manifest = json.loads(manifest_path.read_text())
    if not isinstance(manifest, dict):
        raise ValueError("invalid artifact manifest")
    if manifest.get("version") != 1 or manifest.get("architecture") != architecture:
        raise ValueError("artifact manifest version or architecture mismatch")
    if manifest.get("files") != artifacts(root, architecture):
        raise ValueError("artifact contents do not match their manifest")
    expected = source_digest(repo)
    recorded = manifest.get("source_digest")
    if not isinstance(recorded, str) or re.fullmatch(r"[0-9a-f]{64}", recorded) is None:
        raise ValueError("invalid source digest")
    return {
        "scope": "exact-input" if recorded == expected else "host-only",
        "source_digest": expected,
        "artifact_source_digest": recorded,
        "manifest_verified": True,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["source-key", "seal", "verify"])
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--architecture", choices=["x86_64", "aarch64"], required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.action == "source-key":
        print(source_digest(args.repo))
        return 0
    if args.artifacts is None:
        parser.error("--artifacts is required")
    try:
        if args.action == "seal":
            seal(args.repo, args.artifacts, args.architecture)
        result = verify(args.repo, args.artifacts, args.architecture)
    except (OSError, ValueError) as error:
        result = {"scope": "unavailable", "reason": str(error)}
    encoded = json.dumps(result, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded)
    print(encoded, end="")
    return 0 if result["scope"] in {"exact-input", "host-only"} else 1


if __name__ == "__main__":
    sys.exit(main())
