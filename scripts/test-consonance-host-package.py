#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def run(*args):
    return subprocess.run(args, capture_output=True, text=True, check=False)


def expect(result, status, message):
    if result.returncode != status or message not in result.stderr:
        raise AssertionError(
            f"expected exit {status} with {message!r}; got exit {result.returncode}: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )


def check_binary_shape(host):
    headers = run("readelf", "-l", str(host))
    dynamic = run("readelf", "-d", str(host))
    if headers.returncode or dynamic.returncode:
        raise AssertionError(f"cannot read strict host ELF: {headers.stderr} {dynamic.stderr}")
    if "INTERP" in headers.stdout or "(NEEDED)" in dynamic.stdout:
        raise AssertionError("Linux Consonance host has a dynamic loader or needed library")


def check_pair(frontend, host, platform):
    with tempfile.TemporaryDirectory(prefix="harmony-host-package-") as temporary:
        bin_dir = Path(temporary)
        packaged_frontend = bin_dir / "harmony"
        packaged_host = bin_dir / "harmony-consonance-host"
        shutil.copy2(frontend, packaged_frontend)
        shutil.copy2(host, packaged_host)

        if platform == "linux":
            check_binary_shape(packaged_host)
            expect(run(str(packaged_host), "--version"), 2, "not a sealed memfd")
        else:
            verification = run("codesign", "--verify", "--strict", str(packaged_host))
            if verification.returncode:
                raise AssertionError(f"packaged macOS host signature is invalid: {verification.stderr}")
            version = run(str(packaged_host), "--version")
            if version.returncode != 0 or "harmony" not in version.stdout:
                raise AssertionError(f"signed macOS host was refused: {version}")

        missing_image = str(bin_dir / "missing-issue688-image")
        missing_rom = str(bin_dir / "missing-issue688.nes")
        backend = "kvm" if platform == "linux" else "hvf"
        config = (
            f'[workload]\npackage="faults"\ninput="{missing_image}"\n'
            f'[runner]\nkind="consonance"\nbackend="{backend}"\n'
        )
        error = "image names a path outside its layout or rootfs"
        consonance = (str(packaged_frontend), "check", "--offline", "--config-toml", config)
        expect(run(*consonance), 2, error)

        packaged_host.unlink()
        expect(run(*consonance), 2, "packaged Consonance host is missing")
        native = run(str(packaged_frontend), "check", missing_rom)
        expect(native, 2, "No such file or directory")
        if "packaged Consonance host" in native.stderr:
            raise AssertionError("native QuickNES was routed to the Consonance host")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--frontend", type=Path, required=True)
    parser.add_argument("--host", type=Path, required=True)
    parser.add_argument("--platform", choices=("linux", "macos"), required=True)
    args = parser.parse_args()
    for path in (args.frontend, args.host):
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"executable is unavailable: {path}")
    check_pair(args.frontend.resolve(), args.host.resolve(), args.platform)
    print(f"{args.platform} paired Consonance host package passed")


if __name__ == "__main__":
    main()
