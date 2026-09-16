#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import importlib.util
import json
from pathlib import Path
import shutil

spec = importlib.util.spec_from_file_location("runtime", Path(__file__).with_name("runtime-artifacts.py"))
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)
PROVENANCE = "nix-build-provenance.json"


def inventory(root):
    if root.is_symlink() or not root.is_dir():
        raise ValueError("unsupported artifact root")
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError(f"unsupported prebuilt entry: {path}")
        if path.is_file() and path.relative_to(root).as_posix() != PROVENANCE:
            result[path.relative_to(root).as_posix()] = runtime.digest(path)
    return result


PAYLOAD_MANIFEST = "runtime-payloads.json"
TOOLCHAIN = "nightly-2026-06-16"


def payload_record(repo, root, architecture):
    files = {name: runtime.digest(root / name) for name in ["init.sh", "harmony-supervisor"]}
    value = {"version": 1, "architecture": architecture, "source_digest": runtime.source_digest(repo),
             "toolchain": TOOLCHAIN, "target": architecture + "-unknown-linux-musl", "files": files}
    (root / PAYLOAD_MANIFEST).write_text(json.dumps(value, sort_keys=True, indent=2) + "\n")


def payload_verify(repo, manifest, init, supervisor, architecture):
    if any(path.is_symlink() or not path.is_file() for path in [manifest, init, supervisor]):
        raise ValueError("unsupported runtime payload")
    value = json.loads(manifest.read_text())
    if (value.get("version") != 1 or value.get("architecture") != architecture
            or value.get("toolchain") != TOOLCHAIN
            or value.get("target") != architecture + "-unknown-linux-musl"):
        raise ValueError("unsupported runtime payload manifest")
    if value.get("source_digest") != runtime.source_digest(repo):
        raise ValueError("runtime payload source digest differs")
    if value.get("files") != {"init.sh": runtime.digest(init), "harmony-supervisor": runtime.digest(supervisor)}:
        raise ValueError("runtime payload bytes differ")
    if runtime.digest(init) != runtime.digest(repo / "consonance/harmony-linux/runtime/init.sh"):
        raise ValueError("runtime init differs from canonical source")
    return value


def record(repo, root, architecture):
    required = [f"{architecture}/{name}" for name in
                ("bzImage" if architecture == "x86_64" else "Image", "initramfs-oci.cpio.gz", "initramfs.cpio.gz", "oci-runtime.manifest")]
    files = inventory(root)
    if not all(name in files for name in required + ["MANIFEST.sha256", PAYLOAD_MANIFEST]):
        raise ValueError("Nix output unavailable: missing OCI runtime, direct fixture or build manifest")
    result = {"version": 1, "architecture": architecture,
              "builder": "nix-locked-platform", "source_digest": runtime.source_digest(repo), "files": files}
    (root / PROVENANCE).write_text(json.dumps(result, sort_keys=True, indent=2) + "\n")
    return result


def verify(repo, root, architecture):
    if root.is_symlink() or (root / PROVENANCE).is_symlink():
        raise ValueError("unsupported provenance symlink")
    recorded = json.loads((root / PROVENANCE).read_text())
    if recorded.get("version") != 1 or recorded.get("architecture") != architecture or recorded.get("builder") != "nix-locked-platform":
        raise ValueError("unsupported Nix provenance")
    if recorded.get("source_digest") != runtime.source_digest(repo):
        raise ValueError("prebuilt Nix source digest differs from current source")
    if recorded.get("files") != inventory(root):
        raise ValueError("prebuilt Nix bytes differ from build provenance")
    required = [f"{architecture}/{name}" for name in
                ("bzImage" if architecture == "x86_64" else "Image", "initramfs-oci.cpio.gz", "initramfs.cpio.gz", "oci-runtime.manifest")]
    if not all(name in recorded["files"] for name in required + ["MANIFEST.sha256", PAYLOAD_MANIFEST]):
        raise ValueError("Nix output unavailable: missing OCI runtime, direct fixture or build manifest")
    return recorded


def package(repo, root, output, fixture, architecture):
    verify(repo, root, architecture)
    if output.exists():
        raise ValueError("package output must not exist")
    if fixture.is_symlink() or not fixture.is_dir():
        raise ValueError("missing fixture directory")
    inventory(fixture)
    output.mkdir(parents=True)
    kernel = "bzImage" if architecture == "x86_64" else "Image"
    for name in [kernel, "initramfs-oci.cpio.gz", "initramfs.cpio.gz"]:
        shutil.copyfile(root / architecture / name, output / name)
    shutil.copyfile(root / architecture / "oci-runtime.manifest", output / "oci-runtime.manifest")
    shutil.copytree(fixture, output / "fixture")
    provenance = output / "build-provenance"
    provenance.mkdir()
    for name in (PAYLOAD_MANIFEST, PROVENANCE, "MANIFEST.sha256"):
        shutil.copyfile(root / name, provenance / name)
    # Check again before sealing so changed inputs cannot receive the current key.
    verify(repo, root, architecture)
    for name in [kernel, "initramfs-oci.cpio.gz", "initramfs.cpio.gz"]:
        if runtime.digest(output / name) != runtime.digest(root / architecture / name):
            raise ValueError("copied prebuilt bytes changed")
    runtime.seal(repo, output, architecture)


def main():
    parser = argparse.ArgumentParser(description="Package source-bound Nix OCI bytes without rebuilding the kernel")
    parser.add_argument("action", choices=["record", "verify", "package", "payload-record", "payload-verify"])
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--architecture", required=True, choices=["x86_64", "aarch64"])
    parser.add_argument("--output", type=Path)
    parser.add_argument("--fixture", type=Path)
    parser.add_argument("--init", type=Path)
    parser.add_argument("--supervisor", type=Path)
    args = parser.parse_args()
    try:
        if args.action == "payload-record":
            payload_record(args.repo, args.input, args.architecture)
        elif args.action == "payload-verify":
            if args.init is None or args.supervisor is None:
                parser.error("payload-verify requires --init and --supervisor")
            payload_verify(args.repo, args.input, args.init, args.supervisor, args.architecture)
        elif args.action == "record":
            record(args.repo, args.input, args.architecture)
        elif args.action == "verify":
            verify(args.repo, args.input, args.architecture)
        else:
            if args.output is None or args.fixture is None:
                parser.error("package requires --output and --fixture")
            package(args.repo, args.input, args.output, args.fixture, args.architecture)
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f"FAIL: {error}\n")


if __name__ == "__main__":
    main()
