#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare snapshot encoders with their original routines in one executable."""

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
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-codec-") as scratch:
        root = Path(scratch)
        reference = root / "vm-state-reference"
        shutil.copytree(component / "src", reference / "src")
        for filename, arch in [("codec.rs", "x86"), ("arm64.rs", "arm64")]:
            path = reference / "src" / filename
            source = path.read_text()
            start = source.index("    pub fn encode(&self)")
            end = source.index("    pub fn decode(", start)
            original = (component / f"qualification/reference-{arch}.rs").read_text()
            original = original[original.index("    pub fn encode(&self)"):original.rindex("\n}")] + "\n\n"
            source = source[:start] + original + source[end:]
            if arch == "x86":
                source += "\n" + (component / "qualification/reference-sections.rs").read_text()
            if arch == "arm64":
                source = "use crate::codec::encode_timers;\n" + source
                cached = "        self.encode_with_counter(0)"
                if source.count(cached) != 1:
                    raise SystemExit("Update the control for the changed ARM hash encoder")
                source = source.replace(cached, "        let mut hashed = self.clone();\n        hashed.vtimer.counter = 0;\n        Arm64VmState::encode(&hashed)")
            path.write_text(source)
        manifest = (component / "Cargo.toml").read_text().replace('name = "vm-state"', 'name = "vm-state-reference"', 1)
        (reference / "Cargo.toml").write_text(manifest)
        vmm = component.parent / "vmm-core"
        vmm_reference = root / "vmm-core-reference"
        shutil.copytree(vmm / "src", vmm_reference / "src")
        shutil.copytree(vmm / "contracts", vmm_reference / "contracts")
        vmm_manifest = (vmm / "Cargo.toml").read_text().replace('name = "vmm-core"', 'name = "vmm-core-reference"', 1)
        dependency = 'vm-state = { path = "../vm-state", version = "0.1.0" }'
        if vmm_manifest.count(dependency) != 1:
            raise SystemExit("Update the control for the changed vm-state dependency")
        vmm_manifest = re.sub(r'path = "([^"]+)"', lambda m: 'path = ' + json.dumps(str((vmm / m[1]).resolve())), vmm_manifest)
        vmm_manifest = vmm_manifest.replace('vm-state = { path = ' + json.dumps(str(component)), 'vm-state = { package = "vm-state-reference", path = "../vm-state-reference"')
        (vmm_reference / "Cargo.toml").write_text(vmm_manifest)
        (root / "Cargo.toml").write_text(f'''[workspace]
resolver = "2"
members = ["vm-state-reference", "vmm-core-reference"]
[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"
[workspace.lints.clippy]
all = {{ level = "deny", priority = -1 }}
[package]
name = "qualify-codec"
version = "0.0.0"
edition = "2024"
[dependencies]
vm-state = {{ path = {json.dumps(str(component))} }}
vm-state-reference = {{ path = "vm-state-reference" }}
vmm-core = {{ path = {json.dumps(str(vmm))} }}
vmm-core-reference = {{ path = "vmm-core-reference" }}
vmm-backend = {{ path = {json.dumps(str(component.parent / "vmm-backend"))}, features = ["mock"] }}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        shutil.copyfile(component / "qualification/codec.rs", root / "src/main.rs")
        shutil.copyfile(component / "qualification/vmm.rs", root / "src/vmm.rs")
        target = repo / "target/codec-qualification"
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-codec"
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}), flush=True)
        subprocess.run([str(binary), *(["--check"] if args.check else [])], check=True)


if __name__ == "__main__":
    main()
