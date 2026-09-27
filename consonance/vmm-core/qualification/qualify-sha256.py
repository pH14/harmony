#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Qualify the native and software SHA-256 VMMs in one release executable."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="bounded correctness checks only")
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"], cwd=repo
    ))
    sha = next(p for p in metadata["packages"] if p["name"] == "sha2")
    if sha["version"] != "0.10.9":
        raise SystemExit("Requalify the frozen software control before changing sha2 versions")
    source = Path(sha["manifest_path"]).parent
    archive = source.parents[2] / "cache" / source.parent.name / (source.name + ".crate")
    lock = (repo / "Cargo.lock").read_text()
    expected = re.search(r'name = "sha2"\nversion = "0.10.9"\nsource = "[^\n]+"\nchecksum = "([^\"]+)"', lock)[1]
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise SystemExit("SHA-256 control archive differs from Cargo.lock")
    host = next(line.removeprefix("host: ") for line in subprocess.check_output(
        ["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    with tempfile.TemporaryDirectory(prefix="harmony-sha256-") as scratch:
        root = Path(scratch)
        with tarfile.open(archive) as package:
            package.extractall(root)
        reference = root / "sha2-reference"
        (root / source.name).rename(reference)
        for file in (reference / "src").rglob("*.rs"):
            if file.read_bytes() != (source / file.relative_to(reference)).read_bytes():
                raise SystemExit(f"Modified production sha2 source: {file.name}")
        manifest = reference / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('name = "sha2"', 'name = "sha2-reference"', 1).replace('name = "sha2"', 'name = "sha2_reference"'))
        baseline = root / "vmm-core-reference"
        shutil.copytree(component / "src", baseline / "src")
        shutil.copytree(component / "contracts", baseline / "contracts")
        original = (component / "Cargo.toml").read_text()
        if (original.count('sha2 = "0.10"') != 1
                or original.count('sha2 = { version = "0.10", features = ["asm"] }') != 1):
            raise SystemExit("Update the qualification for the changed SHA-256 dependency declaration")
        frozen = original.replace('name = "vmm-core"', 'name = "vmm-core-reference"', 1)
        frozen = re.sub(r'path = "([^"]+)"',
                        lambda m: 'path = ' + json.dumps(str((component / m[1]).resolve())), frozen)
        frozen = frozen.replace('sha2 = "0.10"',
                                'sha2 = { package = "sha2-reference", path = "../sha2-reference", features = ["force-soft"] }')
        frozen = frozen.replace('sha2 = { version = "0.10", features = ["asm"] }',
                                'sha2 = { package = "sha2-reference", path = "../sha2-reference", features = ["force-soft"] }')
        (baseline / "Cargo.toml").write_text(frozen)
        (root / "Cargo.toml").write_text(f'''[workspace]
resolver = "2"
members = ["vmm-core-reference"]
exclude = ["sha2-reference"]
[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"
[workspace.lints.clippy]
all = {{ level = "deny", priority = -1 }}
[package]
name = "qualify-sha256"
version = "0.0.0"
edition = "2024"
[dependencies]
vmm-core = {{ path = {json.dumps(str(component))} }}
vmm-core-reference = {{ path = "vmm-core-reference" }}
vmm-backend = {{ path = {json.dumps(str(component.parent / "vmm-backend"))}, features = ["mock"] }}
sha2 = "=0.10.9"
sha2-reference = {{ path = "sha2-reference", features = ["force-soft"] }}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        shutil.copyfile(component / "qualification" / "sha256.rs", root / "src" / "main.rs")
        cargo = ["cargo", "--offline", "--manifest-path", str(root / "Cargo.toml")]
        target = Path(metadata["target_directory"]) / "sha256-qualification"
        build = subprocess.run(
            [cargo[0], "build", *cargo[1:], "--release", "--target-dir", str(target),
             "--target", host, "--message-format=json"],
            cwd=repo, text=True, stdout=subprocess.PIPE)
        features = {}
        binary = None
        names = {"sha2": "sha2", "sha2_reference": "sha2-reference"}
        for line in build.stdout.splitlines():
            message = json.loads(line)
            if message["reason"] == "compiler-message":
                print(message["message"]["rendered"], file=sys.stderr, end="")
            if message["reason"] != "compiler-artifact":
                continue
            name = message["target"]["name"]
            if name in names:
                features[names[name]] = message["features"]
            if name == "qualify-sha256" and message.get("executable"):
                binary = Path(message["executable"])
        build.check_returncode()
        if binary is None:
            raise SystemExit("The qualification executable was not built")
        if "force-soft" not in features["sha2-reference"]:
            raise SystemExit("The control was not compiled with software hashing")
        if {"force-soft", "force-soft-compact"}.intersection(features["sha2"]):
            raise SystemExit("The production hasher was compiled with forced software hashing")
        native_arm = host.startswith("aarch64-") and ("linux" in host or "apple-darwin" in host)
        if ("asm" in features["sha2"]) != native_arm:
            raise SystemExit("The compiled SHA-256 backend does not match the native target")
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                          "sha2_version": sha["version"], "sha2_checksum": expected,
                          "host": host, "features": features}), flush=True)
        subprocess.run([str(binary), *(["--check"] if args.check else [])], cwd=repo, check=True)


if __name__ == "__main__":
    main()
