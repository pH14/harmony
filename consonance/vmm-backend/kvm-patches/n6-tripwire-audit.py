#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fail-closed N6 audit of the three retained optional KVM tripwire patches."""

from __future__ import annotations

import hashlib
import re
import sys
from pathlib import Path


REQUIRED = {
    "0001": (
        "KVM_EXIT_DETERMINISM",
        "KVM_CAP_X86_DETERMINISTIC_INTERCEPTS",
        "KVM_DETERMINISM_RDTSC",
        "KVM_DETERMINISM_RDTSCP",
        "KVM_DETERMINISM_RDRAND",
        "KVM_DETERMINISM_RDSEED",
    ),
    "0002": (
        "deterministic_intercepts",
        "kvm_vm_ioctl_enable_cap",
        "KVM_CAP_X86_DETERMINISTIC_INTERCEPTS",
    ),
    "0003": (
        "CPU_BASED_RDTSC_EXITING",
        "SECONDARY_EXEC_RDRAND_EXITING",
        "SECONDARY_EXEC_RDSEED_EXITING",
        "kvm_emulate_rdtsc_intercept",
        "kvm_emulate_rdtscp_intercept",
        "kvm_emulate_rng_intercept",
    ),
}


def pinned_hashes(lock: str) -> dict[str, str]:
    """Extract the exact three SHA-256 pins without accepting duplicates."""
    found: dict[str, str] = {}
    for match in re.finditer(r"^KVM_PATCH_(000[123])_SHA256=([0-9a-f]{64})$", lock, re.M):
        key, digest = match.groups()
        if key in found:
            raise ValueError(f"duplicate hash pin {key}")
        found[key] = digest
    if set(found) != set(REQUIRED):
        raise ValueError("versions.lock does not pin exactly KVM patches 0001-0003")
    return found


def audit(patches: dict[str, bytes], pins: dict[str, str]) -> None:
    """Verify identity and the closure-critical mechanism in every patch."""
    if set(patches) != set(REQUIRED):
        raise ValueError("patch directory does not contain exactly retained patches 0001-0003")
    for key, required in REQUIRED.items():
        data = patches[key]
        digest = hashlib.sha256(data).hexdigest()
        if digest != pins[key]:
            raise ValueError(f"patch {key} hash drift: {digest}, want {pins[key]}")
        text = data.decode("utf-8", errors="strict")
        missing = [token for token in required if token not in text]
        if missing:
            raise ValueError(f"patch {key} missing mechanism tokens: {', '.join(missing)}")


def main() -> int:
    root = Path(__file__).resolve().parents[3]
    patch_root = Path(__file__).resolve().parent / "patches"
    lock_path = root / "consonance" / "harmony-linux" / "linux" / "versions.lock"
    try:
        pins = pinned_hashes(lock_path.read_text())
        files = sorted(patch_root.glob("000[1-3]-*.patch"))
        patches = {path.name[:4]: path.read_bytes() for path in files}
        audit(patches, pins)
        print("N6_TRIPWIRE_OK patches=3 hashes=3 mechanisms=15")

        # Meaningful planted negative: deleting the exit ABI token while
        # recomputing its pin must still fail the semantic audit.
        mutant = dict(patches)
        mutant["0001"] = mutant["0001"].replace(
            b"KVM_EXIT_DETERMINISM", b"KVM_EXIT_REMOVED"
        )
        mutant_pins = dict(pins)
        mutant_pins["0001"] = hashlib.sha256(mutant["0001"]).hexdigest()
        try:
            audit(mutant, mutant_pins)
        except ValueError:
            print("N6_TRIPWIRE_NEGATIVE_OK removed-exit-ABI=rejected")
        else:
            raise ValueError("planted tripwire mechanism deletion passed")
    except (OSError, UnicodeError, ValueError) as error:
        print(f"N6_TRIPWIRE_FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
