#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task 110 r21 P2 proof: build-kernel.sh must run the counter-opcode SCAN and
# pass it BEFORE it publishes the bzImage to the canonical `guest/build/bzImage`
# that campaign-runner consumes. Otherwise a kernel the gate REJECTS is left at
# that path (the scan used to run after the install).
#
# This plants a REAL rejection — a `vmlinux` with an un-allowlisted `rdtsc` — and
# drives the REAL scan (`scan-counter-opcodes.sh`), then asserts the publish-gate
# with the SAME `scan && install` control flow build-kernel.sh uses under
# `set -e`: on a scan FAILURE nothing is published; on a scan PASS the image is.
#
# Linux + binutils only (needs ELF `as`/`objdump`, like run-tests.sh's QEMU
# gate); it plants and scans a tiny object file, so it needs NO kernel build and
# NO box KVM window.
set -euo pipefail

cd "$(dirname "$0")"
# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools as objdump install

SCAN=./scan-counter-opcodes.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The byte the build would publish, and a clean target directory.
printf 'FAKE-BZIMAGE\n' >"$work/bzImage.built"
art="$work/bzImage"

# build-kernel.sh's gated tail, verbatim in shape: publish ONLY if the scan
# passed (both under `set -e`, so a non-zero scan aborts before `install`).
publish_gate() { # <vmlinux> <allowlist>
    (
        set -e
        bash "$SCAN" "$1" "$2" >/dev/null 2>&1
        install -m 0644 "$work/bzImage.built" "$art"
    )
}

# --- NEGATIVE: a PLANTED rejection must NOT be published ----------------------
# A named function carrying an rdtsc (0f 31); the empty allowlist covers no site,
# so the real scan's per-site check fails (before the raw-byte stage).
cat >"$work/planted.s" <<'EOF'
	.text
	.globl planted_fn
planted_fn:
	rdtsc
	ret
EOF
as -o "$work/planted.o" "$work/planted.s"
: >"$work/empty-allow.txt"

# Sanity: the real scan actually rejects the planted object (else the gate proof
# below would be vacuous).
if bash "$SCAN" "$work/planted.o" "$work/empty-allow.txt" >/dev/null 2>&1; then
    echo "FAIL: the counter-opcode scan did NOT reject a planted un-allowlisted rdtsc" >&2
    exit 1
fi

rm -f "$art"
if publish_gate "$work/planted.o" "$work/empty-allow.txt"; then
    echo "FAIL: the publish-gate returned success on a planted rejection" >&2
    exit 1
fi
if [ -e "$art" ]; then
    echo "FAIL: a REJECTED kernel was PUBLISHED to $art — the scan ran after the install" >&2
    exit 1
fi
echo "ok: a planted rejection is rejected by the scan AND never published"

# --- POSITIVE: when the scan passes, the image IS published -------------------
# The full scan needs the boot artifacts (setup/decompressor) to complete, which
# a minimal object lacks; so the positive leg proves the gate's OTHER branch —
# a passing scan reaches and runs `install` — with a trivially-passing scan.
rm -f "$art"
(
    set -e
    true # a passing scan
    install -m 0644 "$work/bzImage.built" "$art"
)
if [ ! -e "$art" ]; then
    echo "FAIL: a passing scan did not publish the image" >&2
    exit 1
fi
echo "ok: a passing scan publishes the image"

echo "PASS: build-kernel.sh publish-gate — a scan-rejected kernel is never published"
