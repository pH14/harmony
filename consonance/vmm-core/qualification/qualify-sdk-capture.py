#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare reused and repeated SDK captures in one executable."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--hvf", action="store_true")
    parser.add_argument("--encoding", action="store_true", help="Compare direct SDK encoding with intermediate buffers")
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-sdk-capture-") as scratch:
        root = Path(scratch)
        reference = root / "vmm-core-reference"
        shutil.copytree(component / "src", reference / "src")
        shutil.copytree(component / "contracts", reference / "contracts")
        if args.encoding:
            path = reference / "src/vmm.rs"
            source = path.read_text()
            direct = "append_sdk_channel(&mut out, sdk, sdk_recorded)?;"
            if source.count(direct) != 1 or source.count("fn append_sdk_channel(") != 1:
                raise SystemExit("Update the reference for the changed SDK encoding")
            source = source.replace(direct, 'put_chunk(&mut out, b"SDK\\0", &encode_sdk_channel(sdk, sdk_recorded)?);')
            start = source.index("fn append_sdk_channel(")
            end = source.index("\n#[cfg(test)]\nmod tests", start)
            original = (component / "qualification/reference-sdk-encoding.rs").read_text()
            source = source[:start] + original + source[end:]
            path.write_text(source)
        else:
            path = reference / "src/control.rs"
            source = path.read_text()
            capture = "sdk_channel.as_ref().map(|channel| &channel.recorded)"
            if source.count(capture) != 1:
                raise SystemExit("Update the reference for the changed SDK capture implementation")
            source = source.replace(capture, "None")
            path.write_text(source)
        manifest = (component / "Cargo.toml").read_text().replace('name = "vmm-core"', 'name = "vmm-core-reference"', 1)
        manifest = re.sub(r'path = "([^\"]+)"', lambda m: 'path = ' + json.dumps(str((component / m[1]).resolve())), manifest)
        (reference / "Cargo.toml").write_text(manifest)
        (root / "Cargo.toml").write_text(f'''[workspace]
resolver = "2"
members = ["vmm-core-reference"]
[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"
[workspace.lints.clippy]
all = {{ level = "deny", priority = -1 }}
[package]
name = "qualify-sdk-capture"
version = "0.0.0"
edition = "2024"
[dependencies]
vmm-core = {{ path = {json.dumps(str(component))} }}
vmm-core-reference = {{ path = "vmm-core-reference" }}
vmm-backend = {{ path = {json.dumps(str(component.parent / "vmm-backend"))}, features = ["mock"] }}
vm-state = {{ path = {json.dumps(str(component.parent / "vm-state"))} }}
control-proto = {{ path = {json.dumps(str(component.parent / "control-proto"))} }}
environment = {{ path = {json.dumps(str(component.parent / "environment"))} }}
vtime = {{ path = {json.dumps(str(component.parent / "vtime"))} }}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        driver = (component / "qualification/sdk-capture.rs").read_text()
        (root / "src/main.rs").write_text(driver)
        target = repo / ("target/sdk-encoding-qualification" if args.encoding else "target/sdk-capture-qualification")
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-sdk-capture"
        command = [str(binary)]
        if args.hvf:
            runner = str(repo / "scripts/macos-hvf-runner.sh")
            subprocess.run([runner, str(binary), "--identity-only"], cwd=repo, check=True)
            command = [runner, *command, "--hvf"]
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                          "allocator": "system",
                          "comparison": "encoding" if args.encoding else "capture"}), flush=True)
        subprocess.run([*command, *(["--check"] if args.check else [])], cwd=repo, check=True)


if __name__ == "__main__":
    main()
