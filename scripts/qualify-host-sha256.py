#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check independent host SHA backends and compare real consumers in one binary."""

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


REPO = Path(__file__).resolve().parent.parent
COMPONENTS = {
    "oci-support": "consonance/oci",
    "unison": "consonance/unison",
    "searcher": "dissonance/searcher",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    host = next(line.removeprefix("host: ") for line in subprocess.check_output(
        ["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    native_arm = host.startswith("aarch64-") and ("linux" in host or "apple-darwin" in host)
    target = REPO / "target/host-sha256-qualification"
    for name, path in COMPONENTS.items():
        result = subprocess.run(["cargo", "build", "--offline", "--locked", "--lib", "--release",
                                 "--manifest-path", str(REPO / path / "Cargo.toml"), "-p", name,
                                 "--target", host, "--target-dir", str(target), "--message-format=json"],
                                cwd=REPO, text=True, stdout=subprocess.PIPE)
        features = None
        for line in result.stdout.splitlines():
            message = json.loads(line)
            if message["reason"] == "compiler-message":
                print(message["message"]["rendered"], file=sys.stderr, end="")
            if message["reason"] == "compiler-artifact" and message["target"]["name"] == "sha2":
                features = message["features"]
        result.check_returncode()
        if features is None or ("asm" in features) != native_arm or {"force-soft", "force-soft-compact"}.intersection(features):
            raise SystemExit(f"Wrong standalone {name} SHA backend on {host}: {features}")
        print(json.dumps({"component": name, "host": host, "compiled_sha2_features": features}), flush=True)
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--offline", "--format-version", "1"], cwd=REPO))
    sha = next(p for p in metadata["packages"] if p["name"] == "sha2")
    if sha["version"] != "0.10.9":
        raise SystemExit("Requalify the software control before changing sha2 versions")
    source = Path(sha["manifest_path"]).parent
    archive = source.parents[2] / "cache" / source.parent.name / (source.name + ".crate")
    expected = re.search(r'name = "sha2"\nversion = "0.10.9"\nsource = "[^\n]+"\nchecksum = "([^\"]+)"', (REPO / "Cargo.lock").read_text())[1]
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise SystemExit("Software control archive differs from Cargo.lock")
    with tempfile.TemporaryDirectory(prefix="harmony-host-sha256-") as scratch:
        root = Path(scratch)
        with tarfile.open(archive) as package:
            package.extractall(root)
        reference = root / "sha2-reference"
        (root / source.name).rename(reference)
        for file in (reference / "src").rglob("*.rs"):
            if file.read_bytes() != (source / file.relative_to(reference)).read_bytes():
                raise SystemExit("Modified production sha2 source")
        manifest = reference / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('name = "sha2"', 'name = "sha2-reference"', 1).replace('name = "sha2"', 'name = "sha2_reference"'))
        for name in ["unison", "searcher"]:
            component = REPO / COMPONENTS[name]
            baseline = root / (name + "-reference")
            shutil.copytree(component / "src", baseline / "src")
            if (component / "benches").exists():
                shutil.copytree(component / "benches", baseline / "benches")
            original = (component / "Cargo.toml").read_text()
            frozen = original.replace(f'name = "{name}"', f'name = "{name}-reference"', 1)
            frozen = re.sub(r'path = "([^"]+)"', lambda m: 'path = ' + json.dumps(str((component / m[1]).resolve())), frozen)
            frozen, count = re.subn(r'^sha2 = .+$', 'sha2 = { package = "sha2-reference", path = "../sha2-reference", features = ["force-soft"] }', frozen, flags=re.MULTILINE)
            if count != 2:
                raise SystemExit(f"Update control for changed {name} SHA declarations")
            (baseline / "Cargo.toml").write_text(frozen)
        (root / "Cargo.toml").write_text(f'''[workspace]
resolver = "2"
members = ["unison-reference", "searcher-reference"]
exclude = ["sha2-reference"]
[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"
[workspace.lints.clippy]
all = {{ level = "deny", priority = -1 }}
[package]
name = "qualify-host-sha256"
version = "0.0.0"
edition = "2024"
[dependencies]
unison = {{ path = {json.dumps(str(REPO / COMPONENTS['unison']))}, default-features = false }}
unison-reference = {{ path = "unison-reference", default-features = false }}
searcher = {{ path = {json.dumps(str(REPO / COMPONENTS['searcher']))} }}
searcher-reference = {{ path = "searcher-reference" }}
''')
        shutil.copyfile(REPO / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        shutil.copyfile(REPO / "scripts/qualification/host-sha256.rs", root / "src/main.rs")
        build = subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                               "--target", host, "--target-dir", str(target), "--message-format=json"],
                              cwd=REPO, text=True, stdout=subprocess.PIPE)
        features = {}
        for line in build.stdout.splitlines():
            message = json.loads(line)
            if message["reason"] == "compiler-message":
                print(message["message"]["rendered"], file=sys.stderr, end="")
            if message["reason"] == "compiler-artifact" and message["target"]["name"] in ["sha2", "sha2_reference"]:
                features[message["target"]["name"]] = message["features"]
        build.check_returncode()
        if "force-soft" not in features.get("sha2_reference", []):
            raise SystemExit("Control did not use software SHA")
        if "sha2" not in features or ("asm" in features["sha2"]) != native_arm or {"force-soft", "force-soft-compact"}.intersection(features.get("sha2", [])):
            raise SystemExit("Benchmark production SHA backend differs from the host")
        binary = target / host / "release/qualify-host-sha256"
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "features": features}), flush=True)
        subprocess.run([str(binary), *(["--check"] if args.check else [])], check=True)


if __name__ == "__main__":
    main()
