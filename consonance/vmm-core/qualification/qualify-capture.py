#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare shared and repeated snapshot captures in one executable."""

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
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-capture-") as scratch:
        root = Path(scratch)
        reference = root / "vmm-core-reference"
        shutil.copytree(component / "src", reference / "src")
        shutil.copytree(component / "contracts", reference / "contracts")
        path = reference / "src/control.rs"
        source = path.read_text()
        shared = "        let (vm_state, vcpu) = match vmm.capture_vm_state() {"
        suffix = "        let mut state_blob_suffix = vmm.state_blob_suffix_from_vcpu(&vcpu)?;\n        drop(vcpu);"
        if source.count(shared) != 1 or source.count(suffix) != 1:
            raise SystemExit("Update the control for the changed capture implementation")
        source = source.replace(shared, "        let vm_state = match vmm.save_vm_state() {")
        source = source.replace(suffix, "        let mut state_blob_suffix = vmm.state_blob_suffix()?;")
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
name = "qualify-capture"
version = "0.0.0"
edition = "2024"
[dependencies]
vmm-core = {{ path = {json.dumps(str(component))} }}
vmm-core-reference = {{ path = "vmm-core-reference" }}
vmm-backend = {{ path = {json.dumps(str(component.parent / "vmm-backend"))}, features = ["mock"] }}
vm-state = {{ path = {json.dumps(str(component.parent / "vm-state"))} }}
control-proto = {{ path = {json.dumps(str(component.parent / "control-proto"))} }}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        shutil.copyfile(component / "qualification/capture.rs", root / "src/main.rs")
        target = repo / "target/capture-qualification"
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-capture"
        command = [str(binary)]
        if args.hvf:
            runner = str(repo / "scripts/macos-hvf-runner.sh")
            subprocess.run([runner, str(binary), "--identity-only"], cwd=repo, check=True)
            command = [runner, *command, "--hvf"]
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}), flush=True)
        subprocess.run([*command, *(["--check"] if args.check else [])], cwd=repo, check=True)


if __name__ == "__main__":
    main()
