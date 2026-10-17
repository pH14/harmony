#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "consonance/host-code-identity/tests/witness/Cargo.toml"


def clean_environment():
    return {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("LD_") and key != "GLIBC_TUNABLES"
    }


def build_fixture():
    command = [
        "cargo",
        "build",
        "--locked",
        "--manifest-path",
        str(FIXTURE),
        "--target-dir",
        str(ROOT / "target"),
        "--message-format=json",
    ]
    result = subprocess.run(command, cwd=ROOT, env=clean_environment(), capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"fixture build failed:\n{result.stderr}\n{result.stdout[-4000:]}")
    binary = None
    output = None
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "loaded-code-witness" not in message.get("package_id", ""):
            continue
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            binary = Path(message["executable"])
        if message.get("reason") == "build-script-executed":
            output = Path(message["out_dir"])
    if binary is None or output is None:
        raise RuntimeError("fixture build did not report its binary and library directory")
    return binary, output


def verify_dynamic_link(binary):
    result = subprocess.run(["readelf", "-d", str(binary)], capture_output=True, text=True, check=True)
    needed = [line for line in result.stdout.splitlines() if "(NEEDED)" in line]
    witness = next((index for index, line in enumerate(needed) if "libwitness.so" in line), None)
    libc = next((index for index, line in enumerate(needed) if "libc.so" in line), None)
    if witness is None or libc is None or witness >= libc:
        raise AssertionError(f"ordinary witness dependency or link order is missing:\n{result.stdout}")
    if not any("(RUNPATH)" in line and "$ORIGIN" in line for line in result.stdout.splitlines()):
        raise AssertionError(f"$ORIGIN RUNPATH is missing:\n{result.stdout}")


def rodata_variant(library, output):
    original = library.read_bytes()
    marker = b"HARMONY_RODATA_MARKER_A"
    if original.count(marker) != 1:
        raise AssertionError("read-only witness marker is not unique")
    position = original.index(marker)
    phoff = struct.unpack_from("<Q", original, 32)[0]
    phentsize = struct.unpack_from("<H", original, 54)[0]
    phnum = struct.unpack_from("<H", original, 56)[0]
    pure_read_only = False
    for index in range(phnum):
        kind, flags, offset, _, _, filesz, _, _ = struct.unpack_from(
            "<IIQQQQQQ", original, phoff + index * phentsize
        )
        if kind == 1 and flags == 4 and offset <= position < offset + filesz:
            pure_read_only = True
            break
    if not pure_read_only:
        raise AssertionError("witness marker is not in a pure read-only ELF load segment")
    changed = original.replace(marker, b"HARMONY_RODATA_MARKER_B")
    if sum(left != right for left, right in zip(original, changed)) != 1:
        raise AssertionError("read-only witness changes more than one byte")
    output.write_bytes(changed)


def run(binary, scratch, label, preload=None, extra_env=None):
    environment = clean_environment()
    if preload is not None:
        environment["LD_PRELOAD"] = str(preload)
    if extra_env:
        environment.update(extra_env)
    work = scratch / f"work-{label}"
    work.mkdir(exist_ok=True)
    recording = scratch / f"{label}.json"
    result = subprocess.run(
        [str(binary), "record", "-", str(work), str(recording)],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
    )
    (scratch / f"{label}.stdout").write_text(result.stdout)
    (scratch / f"{label}.stderr").write_text(result.stderr)
    return result, recording


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--keep", action="store_true", help="retain scratch output")
    arguments = parser.parse_args()
    if sys.platform != "linux":
        raise SystemExit("loaded-code witness runs on Linux")
    binary, libraries = build_fixture()
    verify_dynamic_link(binary)
    scratch = Path(tempfile.mkdtemp(prefix="harmony-loaded-code-regression-"))
    try:
        run_binary = scratch / "witness"
        shutil.copy2(binary, run_binary)
        library = scratch / "libwitness.so"

        def require_refused(label, preload=None, extra_env=None):
            result, recording = run(
                run_binary,
                scratch,
                label,
                preload=preload,
                extra_env=extra_env,
            )
            if (
                result.returncode == 0
                or "host executable is not a sealed memfd" not in result.stderr
                or recording.exists()
            ):
                raise AssertionError(
                    f"{label} dynamic host was admitted:\n{result.stdout}\n{result.stderr}"
                )
            return result

        shutil.copy2(libraries / "A/libwitness.so", library)
        require_refused("ordinary-library-A")
        shutil.copy2(libraries / "B/libwitness.so", library)
        require_refused("ordinary-library-B")
        shutil.copy2(libraries / "A/libwitness.so", library)
        preload = require_refused(
            "constructor-cleared-preload", preload=libraries / "preload/libclear.so"
        )
        if "preload constructor cleared LD_PRELOAD" not in preload.stderr:
            raise AssertionError("preload constructor did not run")
        anonymous = require_refused(
            "anonymous-executable", extra_env={"HARMONY_LOADED_CODE_ANON_EXEC": "1"}
        )
        if "anonymous executable witness mapped" not in anonymous.stderr:
            raise AssertionError("anonymous executable witness did not map")

        rodata_a = libraries / "rodata/libwitness.so"
        rodata_b = scratch / "libwitness-rodata-B.so"
        rodata_variant(rodata_a, rodata_b)
        shutil.copy2(rodata_a, library)
        require_refused("readonly-library-A")
        shutil.copy2(rodata_b, library)
        require_refused("readonly-library-B")
        print(
            "ordinary dynamic libraries, constructor-cleared preload, anonymous code, and "
            "read-only library variants all refused by public Session identity"
        )
        if arguments.keep:
            print(f"scratch={scratch}")
    finally:
        if not arguments.keep:
            shutil.rmtree(scratch)

if __name__ == "__main__":
    main()
