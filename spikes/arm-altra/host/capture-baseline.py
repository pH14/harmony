#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Capture the Altra box baseline manifest (docs/ARM-ALTRA.md §Box discipline).

Record-then-modify: this manifest is the restore target for the box whenever the
spike lock is yielded. Emits stable JSON (sorted keys) on stdout; every value is
read from the live box, never invented. Run as: sudo python3 capture-baseline.py
(dmidecode needs root; everything else does not).

No box identifiers beyond the `harmony-arm` alias appear in the output (task 122
§Environment): no hostnames, no IPs, no serials.
"""

import glob
import hashlib
import json
import subprocess
import sys


def read(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError as e:
        return f"UNREADABLE: {e.strerror}"


def run(*argv):
    try:
        out = subprocess.run(argv, capture_output=True, text=True, timeout=30)
        if out.returncode != 0:
            return f"FAILED rc={out.returncode}: {out.stderr.strip()}"
        return out.stdout.strip()
    except OSError as e:
        return f"FAILED: {e}"


def sha256_file(path):
    h = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError as e:
        return f"UNREADABLE: {e.strerror}"


def governors():
    """Per-core governor, collapsed: {governor: [cores]}."""
    out = {}
    for p in sorted(glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor")):
        core = int(p.split("/cpu")[2].split("/")[0])
        out.setdefault(read(p), []).append(core)
    return {g: f"{min(c)}-{max(c)}" if len(c) == max(c) - min(c) + 1 else sorted(c)
            for g, c in out.items()}


def main():
    boot_kernel = f"/boot/vmlinuz-{run('uname', '-r')}"
    manifest = {
        "box": "harmony-arm",
        "captured_utc": run("date", "-u", "+%Y-%m-%dT%H:%M:%SZ"),
        "soc": {
            "midr_el1": read(
                "/sys/devices/system/cpu/cpu0/regs/identification/midr_el1"
            ),
            "core_count_online": read("/sys/devices/system/cpu/online"),
            "smt": read("/sys/devices/system/cpu/smt/control"),
            "product": run("dmidecode", "-s", "system-product-name"),
            "processor_version": run("dmidecode", "-s", "processor-version"),
        },
        "firmware": {
            "bios_version": run("dmidecode", "-s", "bios-version"),
            "bios_release_date": run("dmidecode", "-s", "bios-release-date"),
        },
        "kernel": {
            "release": run("uname", "-r"),
            "version": run("uname", "-v"),
            "arch": run("uname", "-m"),
            "cmdline": read("/proc/cmdline"),
            "boot_image": boot_kernel,
            "boot_image_sha256": sha256_file(boot_kernel),
            "kvm": "built-in (CONFIG_KVM=y, stock Ubuntu; no kvm.ko)",
            "kvm_arm_mode": read("/sys/module/kvm_arm/parameters/mode"),
        },
        "runtime_posture": {
            "dmesg_restrict": read("/proc/sys/kernel/dmesg_restrict"),
            "perf_event_paranoid": read("/proc/sys/kernel/perf_event_paranoid"),
            "perf_event_paranoid_note": (
                "set to -1 at provisioning (2026-07-17, runtime sysctl, not "
                "persisted); re-set after any reboot"
            ),
            "governors": governors(),
            "nohz_full": read("/sys/devices/system/cpu/nohz_full"),
            "isolated": read("/sys/devices/system/cpu/isolated"),
        },
        "provisioning_applied_2026_07_17": {
            "note": (
                "captured AFTER Paul's day-one provisioning; the restore target "
                "includes these"
            ),
            "apt_installed": [
                "build-essential", "git", "curl", "pkg-config", "libssl-dev",
            ],
            "rustup": run(
                "sudo", "-u", "ubuntu", "-H", "/home/ubuntu/.cargo/bin/rustc",
                "--version",
            ),
            "kvm_group": "user ubuntu added to group kvm",
            "services_touched": [],
        },
    }
    json.dump(manifest, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
