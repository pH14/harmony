#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare reused and repeated control-state encoding in one executable."""

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
    parser.add_argument("--inputs", action="store_true", help="Compare direct nested input encoding with temporary buffers")
    parser.add_argument("--jemalloc", action="store_true", help="Use the Harmony CLI allocator")
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-control-capture-") as scratch:
        root = Path(scratch)
        reference = root / "vmm-core-reference"
        shutil.copytree(component / "src", reference / "src")
        shutil.copytree(component / "contracts", reference / "contracts")
        if args.inputs:
            path = reference / "src/control_state.rs"
            source = path.read_text()
            for current in ["self.recorded.encode()", "self.pending.encode()"]:
                if source.count(current) != 1:
                    raise SystemExit("Update the reference for the changed control codec")
                source = source.replace(current, current.replace(".encode()", ".encode_buffered()"))
            source += "\n" + (component.parent / "environment/qualification/reference-input-spec.rs").read_text()
            path.write_text(source)
        else:
            path = reference / "src/control.rs"
            source = path.read_text()
            captured = "        let control_state = control.encode_and_append_hash(&mut state_blob_suffix);"
            stored = "                control_state,"
            if source.count(captured) != 1 or source.count(stored) != 1:
                raise SystemExit("Update the reference for the changed control capture implementation")
            source = source.replace(captured, "        control.encode_and_append_hash(&mut state_blob_suffix);")
            source = source.replace(stored, "                control_state: control.encode(),")
            path.write_text(source)
        manifest = (component / "Cargo.toml").read_text().replace('name = "vmm-core"', 'name = "vmm-core-reference"', 1)
        manifest = re.sub(r'path = "([^\"]+)"', lambda m: 'path = ' + json.dumps(str((component / m[1]).resolve())), manifest)
        (reference / "Cargo.toml").write_text(manifest)
        allocator_dependency = 'tikv-jemallocator = "=0.7.0"' if args.jemalloc else ''
        (root / "Cargo.toml").write_text(f'''[workspace]
resolver = "2"
members = ["vmm-core-reference"]
[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"
[workspace.lints.clippy]
all = {{ level = "deny", priority = -1 }}
[package]
name = "qualify-control-capture"
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
{allocator_dependency}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        driver = (component / "qualification/control-capture.rs").read_text()
        if args.jemalloc:
            driver = '#[global_allocator]\nstatic ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;\n' + driver
        (root / "src/main.rs").write_text(driver)
        target = repo / ("target/input-encoding-qualification" if args.inputs else "target/control-capture-qualification")
        if args.jemalloc:
            target = target.with_name(target.name + "-jemalloc")
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-control-capture"
        command = [str(binary)]
        if args.hvf:
            runner = str(repo / "scripts/macos-hvf-runner.sh")
            subprocess.run([runner, str(binary), "--identity-only"], cwd=repo, check=True)
            command = [runner, *command, "--hvf"]
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                          "allocator": "jemalloc" if args.jemalloc else "system",
                          "comparison": "inputs" if args.inputs else "control"}), flush=True)
        subprocess.run([*command, *(["--check"] if args.check else [])], cwd=repo, check=True)


if __name__ == "__main__":
    main()
