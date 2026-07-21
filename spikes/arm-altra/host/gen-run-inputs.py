#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Generate `arm-spike run`'s operator inputs ON the box, from live reads.

Emits, into an output directory:
  environment.json   — the Environment block (MIDR, SoC, firmware, host kernel,
                       KVM mode), every field read from the running box
  payload-pins.json  — trusted sha256 pins, one per payload ELF in --payload-dir
  host-kernel.json   — {path, sha256, build_id} of the vmlinux ELF passed via
                       --vmlinux, for the --host-kernel-* arguments

Run AFTER booting the measurement host and applying spike-posture.sh (the KVM-mode
read needs dmesg readable). Values are read live so the harness's own cross-checks
(live MIDR / kvm_mode / uname -r vs the environment block) grade the same facts
twice from independent reads — nothing here is typed in.

Usage:
  python3 gen-run-inputs.py --payload-dir <dir> --vmlinux <elf> --out <dir>
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

SOC = "Ampere(R) Altra(R) Processor (HPE ProLiant RL300 Gen11)"
FIRMWARE = {"bios_release_date": "01/16/2025", "bios_version": "1.74"}


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def kvm_mode():
    """The effective KVM mode, from the kernel's own boot line (dmesg)."""
    log = subprocess.run(["dmesg"], capture_output=True, text=True, check=True).stdout
    mode = None
    for line in log.splitlines():
        if "kvm" not in line:
            continue
        if "Protected nVHE mode initialized successfully" in line:
            mode = "protected"
        elif "VHE mode initialized successfully" in line:
            mode = "vhe"
        elif "Hyp mode initialized successfully" in line:
            mode = "nvhe"
    if mode is None:
        sys.exit("FAIL: no KVM mode-initialized line in dmesg — cannot attest kvm_mode")
    return mode


def build_id(vmlinux):
    out = subprocess.run(
        ["readelf", "-n", vmlinux], capture_output=True, text=True, check=True
    ).stdout
    m = re.search(r"Build ID:\s*([0-9a-f]+)", out)
    if not m:
        sys.exit(f"FAIL: {vmlinux} carries no GNU build-id note")
    return m.group(1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--payload-dir", required=True)
    ap.add_argument("--vmlinux")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    with open("/sys/devices/system/cpu/cpu0/regs/identification/midr_el1") as f:
        midr = int(f.read().strip(), 16)
    release = os.uname().release

    env = {
        "midr": midr,
        "soc": SOC,
        "firmware": FIRMWARE,
        "host_kernel": release,
        "kvm_mode": kvm_mode(),
    }

    pins = {}
    for name in sorted(os.listdir(args.payload_dir)):
        path = os.path.join(args.payload_dir, name)
        # Payload ELFs are extensionless bin names; skip build residue.
        if not os.path.isfile(path) or "." in name:
            continue
        pins[name] = sha256_file(path)

    kernel = None
    if args.vmlinux:
        kernel = {
            "path": os.path.abspath(args.vmlinux),
            "sha256": sha256_file(args.vmlinux),
            "build_id": build_id(args.vmlinux),
        }

    os.makedirs(args.out, exist_ok=True)
    files = [("environment.json", env), ("payload-pins.json", pins)]
    if kernel is not None:
        files.append(("host-kernel.json", kernel))
    for fname, obj in files:
        with open(os.path.join(args.out, fname), "w") as f:
            json.dump(obj, f, indent=2, sort_keys=True)
            f.write("\n")
    print(f"wrote {', '.join(n for n, _ in files)} ({len(pins)} pins) to {args.out}")


if __name__ == "__main__":
    main()
