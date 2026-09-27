#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare cached and uncached contract fingerprints in one executable."""

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
    parser.add_argument("--architecture", choices=["x86", "arm64"], default="x86")
    parser.add_argument("--system-allocator", action="store_true", help="time with the normal allocator instead of counting allocations")
    args = parser.parse_args()
    if args.miri and args.system_allocator:
        parser.error("Miri must exercise the counting allocator")
    component = Path(__file__).resolve().parent.parent
    repo = component.parent.parent
    with tempfile.TemporaryDirectory(prefix="harmony-contract-") as scratch:
        root = Path(scratch)
        reference = root / "vmm-core-reference"
        shutil.copytree(component / "src", reference / "src")
        shutil.copytree(component / "contracts", reference / "contracts")
        if args.architecture == "x86":
            path = reference / "src/vendor/x86/contract/mod.rs"
            cached = '    static HASH: OnceLock<[u8; 32]> = OnceLock::new();\n    *HASH.get_or_init(compute_contract_hash)'
            replacement = '    compute_contract_hash()'
        else:
            path = reference / "src/vendor/arm64/contract.rs"
            cached = '    static EIGHT: OnceLock<[u8; 32]> = OnceLock::new();\n    static SIXTEEN: OnceLock<[u8; 32]> = OnceLock::new();\n    let hash = match asid_bits {\n        Arm64AsidBits::Eight => &EIGHT,\n        Arm64AsidBits::Sixteen => &SIXTEEN,\n    };\n    *hash.get_or_init(|| compute_contract_hash(asid_bits))'
            replacement = '    compute_contract_hash(asid_bits)'
        source = path.read_text()
        if source.count(cached) != 1:
            raise SystemExit("Update the control for the changed contract cache implementation")
        path.write_text(source.replace(cached, replacement))
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
        driver = (component / "qualification/contract.rs").read_text()
        if args.system_allocator:
            registration = '#[global_allocator]\nstatic ALLOCATOR: CountingAllocator = CountingAllocator;'
            if driver.count(registration) != 1:
                raise SystemExit("Update the system-allocator registration removal")
            driver = driver.replace(registration, '')
        (root / "src/main.rs").write_text(driver)
        shutil.copyfile(component / "qualification/arm64_contract.rs", root / "src/arm64_contract.rs")
        target = repo / "target/contract-qualification"
        if args.miri:
            subprocess.run(["cargo", "+nightly", "miri", "run", "--offline", "--manifest-path", str(root / "Cargo.toml"),
                            "--target-dir", str(target), "--", "--allocator-check"], cwd=root, check=True)
            return
        subprocess.run(["cargo", "build", "--offline", "--release", "--manifest-path", str(root / "Cargo.toml"),
                        "--target-dir", str(target)], cwd=repo, check=True)
        binary = target / "release/qualify-contract"
        print(json.dumps({"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                          "architecture": args.architecture,
                          "allocator": "system" if args.system_allocator else "counting"}), flush=True)
        command = [str(binary), *(["--arm64"] if args.architecture == "arm64" else [])]
        subprocess.run([*command, "--concurrent"], check=True)
        for sample in range(1 if args.check else 9):
            for arm in (["cached", "uncached"] if sample % 2 else ["uncached", "cached"]):
                subprocess.run([*command, "--first-use", arm], check=True)
        subprocess.run([*command, *(["--check"] if args.check else [])], check=True)


if __name__ == "__main__":
    main()
