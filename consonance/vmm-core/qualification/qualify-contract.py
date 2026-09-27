#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare cached and uncached x86 contract fingerprints in one executable."""

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
    parser.add_argument("--miri", action="store_true", help="exercise allocation instrumentation under Miri")
    args = parser.parse_args()
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-contract-") as scratch:
        root = Path(scratch)
        reference = root / "vmm-core-reference"
        shutil.copytree(component / "src", reference / "src")
        shutil.copytree(component / "contracts", reference / "contracts")
        path = reference / "src/vendor/x86/contract/mod.rs"
        cached = '    static HASH: OnceLock<[u8; 32]> = OnceLock::new();\n    *HASH.get_or_init(compute_contract_hash)'
        source = path.read_text()
        if source.count(cached) != 1:
            raise SystemExit("Update the control for the changed contract cache implementation")
        path.write_text(source.replace(cached, '    compute_contract_hash()'))
        manifest = (component / "Cargo.toml").read_text().replace('name = "vmm-core"', 'name = "vmm-core-reference"', 1)
        manifest = re.sub(r'path = "([^"]+)"', lambda m: 'path = ' + json.dumps(str((component / m[1]).resolve())), manifest)
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
name = "qualify-contract"
version = "0.0.0"
edition = "2024"
[dependencies]
vmm-core = {{ path = {json.dumps(str(component))} }}
vmm-core-reference = {{ path = "vmm-core-reference" }}
vmm-backend = {{ path = {json.dumps(str(component.parent / "vmm-backend"))}, features = ["mock"] }}
''')
        shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
        (root / "src").mkdir()
        shutil.copyfile(component / "qualification/contract.rs", root / "src/main.rs")
        target = repo / "target/contract-qualification"
        if args.miri:
            subprocess.run(["cargo", "+nightly", "miri", "run", "--offline", "--manifest-path", str(root / "Cargo.toml"),
                            "--target-dir", str(target), "--", "--allocator-check"], cwd=root, check=True)
            return
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-contract"
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}), flush=True)
        subprocess.run([str(binary), "--concurrent"], check=True)
        for sample in range(1 if args.check else 9):
            for arm in (["cached", "uncached"] if sample % 2 else ["uncached", "cached"]):
                subprocess.run([str(binary), "--first-use", arm], check=True)
        subprocess.run([str(binary), *(["--check"] if args.check else [])], check=True)


if __name__ == "__main__":
    main()
