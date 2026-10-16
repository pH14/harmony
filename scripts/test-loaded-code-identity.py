#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import hashlib
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


def fake_profile(directory):
    directory.mkdir()
    kernel = bytearray(64)
    kernel[:4] = b"\x7fELF"
    kernel[4:6] = b"\x02\x01"
    struct.pack_into("<Q", kernel, 0x20, 64)
    struct.pack_into("<H", kernel, 0x36, 56)
    artifacts = {"linux": bytes(kernel), "config": b"witness", "initramfs.cpio.gz": b"witness"}
    for name, content in artifacts.items():
        (directory / name).write_bytes(content)
    profile = {
        "schema": 4,
        "architecture": os.uname().machine,
        "kernel_version": "loaded-code-witness",
        "userspace": "seccomp",
        "patch_series_sha256": "0" * 64,
        "host_libraries": [],
        "executable": {"name": "linux", "sha256": hashlib.sha256(artifacts["linux"]).hexdigest()},
        "config": {"name": "config", "sha256": hashlib.sha256(artifacts["config"]).hexdigest()},
        "rootfs": {"name": "initramfs.cpio.gz", "sha256": hashlib.sha256(artifacts["initramfs.cpio.gz"]).hexdigest()},
        "virtual_time": {"syscall_vns": 100000, "clock_read_vns": 1},
    }
    (directory / "profile.json").write_text(json.dumps(profile))
    return directory


def run(binary, profile, scratch, label, preload=None, guest=False, mode="record", prior=None, extra_env=None):
    environment = clean_environment()
    if preload is not None:
        environment["LD_PRELOAD"] = str(preload)
    if guest:
        environment["HARMONY_LOADED_CODE_RUN_GUEST"] = "1"
    if extra_env:
        environment.update(extra_env)
    work = scratch / f"work-{label}"
    work.mkdir(exist_ok=True)
    recording = prior if prior is not None else scratch / f"{label}.json"
    result = subprocess.run(
        [str(binary), mode, str(profile), str(work), str(recording)],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
    )
    (scratch / f"{label}.stdout").write_text(result.stdout)
    (scratch / f"{label}.stderr").write_text(result.stderr)
    return result, recording


def require_success(result, label):
    if result.returncode:
        raise AssertionError(f"{label} failed:\n{result.stdout}\n{result.stderr}")
    return json.loads(result.stdout.strip().splitlines()[-1])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", type=Path, help="existing values UML profile for an optional guest-event run")
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
        profile = arguments.profile.resolve() if arguments.profile else fake_profile(scratch / "profile")
        guest = arguments.profile is not None

        shutil.copy2(libraries / "A/libwitness.so", library)
        a_result, a_recording = run(run_binary, profile, scratch, "A", guest=guest)
        a = require_success(a_result, "A")

        shutil.copy2(libraries / "B/libwitness.so", library)
        b_result, _ = run(run_binary, profile, scratch, "B", guest=guest)
        b = require_success(b_result, "B")
        replay_result, _ = run(run_binary, profile, scratch, "B-check-A", mode="check", prior=a_recording)
        if (replay_result.returncode == 0
                or "Executable {" not in replay_result.stderr
                or a["host_executable_identity"] not in replay_result.stderr
                or b["host_executable_identity"] not in replay_result.stderr):
            raise AssertionError(f"B admitted A recording:\n{replay_result.stdout}\n{replay_result.stderr}")

        shutil.copy2(libraries / "A/libwitness.so", library)
        repeat_result, _ = run(run_binary, profile, scratch, "A-repeat", guest=guest)
        repeat = require_success(repeat_result, "A-repeat")

        preload_result, _ = run(run_binary, profile, scratch, "preload-clear", preload=libraries / "preload/libclear.so", guest=guest)
        preload = require_success(preload_result, "constructor-cleared preload")
        if "preload constructor cleared LD_PRELOAD" not in preload_result.stderr:
            raise AssertionError("preload constructor did not run")

        anonymous_result, _ = run(run_binary, profile, scratch, "anonymous-executable", extra_env={"HARMONY_LOADED_CODE_ANON_EXEC": "1"})
        if (anonymous_result.returncode == 0
                or "anonymous executable witness mapped" not in anonymous_result.stderr
                or "unsupported executable mapping" not in anonymous_result.stderr):
            raise AssertionError(f"anonymous executable map was admitted:\n{anonymous_result.stdout}\n{anonymous_result.stderr}")

        for key in ["profile_identity_sha256", "host_executable_identity", "session_identity", "event_hash", "events"]:
            if a[key] != repeat[key]:
                raise AssertionError(f"A-repeat changed {key}")
        for key in ["host_executable_identity", "session_identity"]:
            if a[key] == b[key] or a[key] == preload[key]:
                raise AssertionError(f"loaded code did not change {key}")
        for label, record in [("A", a), ("B", b), ("A-repeat", repeat), ("preload", preload)]:
            if record["selectors"]:
                raise AssertionError(f"{label} still has ambient loader selectors")
        if guest:
            if not a["harmony_entropy"] or not b["harmony_entropy"]:
                raise AssertionError("values fixture produced no entropy event")
            if a["harmony_entropy"] == b["harmony_entropy"] or a["event_hash"] == b["event_hash"]:
                raise AssertionError("witness did not change guest output")

        rodata_a = libraries / "rodata/libwitness.so"
        rodata_b = scratch / "libwitness-rodata-B.so"
        rodata_variant(rodata_a, rodata_b)
        shutil.copy2(rodata_a, library)
        ro_a_result, ro_a_recording = run(run_binary, profile, scratch, "rodata-A", guest=guest)
        ro_a = require_success(ro_a_result, "read-only data A")
        ro_repeat_result, _ = run(run_binary, profile, scratch, "rodata-A-repeat", guest=guest)
        ro_repeat = require_success(ro_repeat_result, "read-only data A-repeat")
        copied = scratch / "copied"
        copied.mkdir()
        copied_binary = copied / "witness"
        shutil.copy2(run_binary, copied_binary)
        shutil.copy2(rodata_a, copied / "libwitness.so")
        ro_copied_result, _ = run(copied_binary, profile, scratch, "rodata-A-copied", guest=guest)
        ro_copied = require_success(ro_copied_result, "read-only data path copy")
        shutil.copy2(rodata_b, library)
        ro_b_result, _ = run(run_binary, profile, scratch, "rodata-B", guest=guest)
        ro_b = require_success(ro_b_result, "read-only data B")
        ro_check_result, _ = run(run_binary, profile, scratch, "rodata-B-check-A", mode="check", prior=ro_a_recording)
        if ro_check_result.returncode == 0 or "Executable {" not in ro_check_result.stderr:
            raise AssertionError(f"read-only data B admitted A recording:\n{ro_check_result.stdout}\n{ro_check_result.stderr}")
        for key in ["host_executable_identity", "session_identity", "event_hash"]:
            if ro_a[key] != ro_repeat[key] or ro_a[key] != ro_copied[key]:
                raise AssertionError(f"read-only data A repeat or path copy changed {key}")
        for key in ["host_executable_identity", "session_identity"]:
            if ro_a[key] == ro_b[key]:
                raise AssertionError(f"read-only data A/B kept the same {key}")
        if guest and (ro_a["event_hash"] == ro_b["event_hash"] or ro_a["harmony_entropy"] == ro_b["harmony_entropy"]):
            raise AssertionError("read-only data A/B did not change guest events")
        print("ordinary DT_NEEDED libwitness.so and $ORIGIN RUNPATH verified")
        print("A/B loaded executable and session identities differ; A-repeat matches; B replay of A refused")
        print("constructor-cleared LD_PRELOAD admitted with a distinct mapped-code identity")
        print("anonymous executable mapping refused at admission")
        print("one-byte read-only library data changes guest output and identity; replay refused")
        print("read-only data A-repeat and copied-path identities match")
        if guest:
            print("values fixture guest entropy and event hash differ under B")
        if arguments.keep:
            print(f"scratch={scratch}")
    finally:
        if not arguments.keep:
            shutil.rmtree(scratch)


if __name__ == "__main__":
    main()
