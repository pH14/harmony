#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare borrowed and cloned SDK snapshot restores in one executable."""

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
    parser.add_argument("--filter", help="Run cases whose name contains this substring")
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-restore-") as scratch:
        root = Path(scratch)
        reference = root / "vmm-core-reference"
        shutil.copytree(component / "src", reference / "src")
        shutil.copytree(component / "contracts", reference / "contracts")
        path = reference / "src/control.rs"
        source = path.read_text()
        borrowed = """        let sdk_snap = self.sdk_snaps.get(&snap.0);
        let restore_policy = env_policy.or_else(|| sdk_snap.as_ref().map(|s| s.policy.clone()));
        if let Some(policy) = restore_policy {
            self.recorded.set_config(policy);
        }"""
        cloned = """        let sdk_snap = self.sdk_snaps.get(&snap.0).cloned();
        let restore_policy = env_policy.or_else(|| sdk_snap.as_ref().map(|s| s.policy.clone()));
        if let Some(policy) = restore_policy {
            self.set_recorded_policy(policy);
        }"""
        if source.count(borrowed) != 1 or source.count("struct SdkSnap {") != 1:
            raise SystemExit("Update the reference for the changed restore implementation")
        source = source.replace(borrowed, cloned)
        hook = "    fn capture_control_state(&self) -> ControlState {"
        if source.count(hook) != 1:
            raise SystemExit("Update the reference for the changed policy setter")
        source = source.replace(hook, """    fn set_recorded_policy(&mut self, policy: ServiceConfig) {
        self.recorded.set_config(policy);
    }

""" + hook)
        source = source.replace("struct SdkSnap {", "#[derive(Clone)]\nstruct SdkSnap {")
        prepared = "            vmm.sdk_restore_events(&s.channel);"
        repeated = """            if seed.is_some() {
                vmm.sdk_restore_events(&s.channel);
            } else {
                if let Err(error) = vmm.sdk_restore(&s.channel) {
                    self.vmm = None;
                    return Err(ServeError::Service(error));
                }
            }"""
        if source.count(prepared) != 1:
            raise SystemExit("Update the reference for the changed SDK commit")
        source = source.replace(prepared, repeated)
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
name = "qualify-restore"
version = "0.0.0"
edition = "2024"
[dependencies]
vmm-core = {{ path = {json.dumps(str(component))} }}
vmm-core-reference = {{ path = "vmm-core-reference" }}
vmm-backend = {{ path = {json.dumps(str(component.parent / "vmm-backend"))}, features = ["mock"] }}
control-proto = {{ path = {json.dumps(str(component.parent / "control-proto"))} }}
environment = {{ path = {json.dumps(str(component.parent / "environment"))} }}
vtime = {{ path = {json.dumps(str(component.parent / "vtime"))} }}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        shutil.copyfile(component / "qualification/restore.rs", root / "src/main.rs")
        target = repo / "target/restore-qualification"
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-restore"
        command = [str(binary), *(["--filter", args.filter] if args.filter else [])]
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}), flush=True)
        subprocess.run([*command, *(["--check"] if args.check else [])], cwd=repo, check=True)


if __name__ == "__main__":
    main()
