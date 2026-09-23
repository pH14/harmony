#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Boot the shipped x86 kernel without Harmony's clock doorbell and require a
# registration panic before the initramfs can start.
set -euo pipefail

caller_dir=$PWD
cd "$(dirname "$0")"
. ./lib-build.sh

require_tools qemu-system-x86_64

kernel=${1:-$ART_DIR/bzImage}
initramfs=${2:-$ART_DIR/initramfs.cpio.gz}
case $kernel in /*) ;; *) kernel=$caller_dir/$kernel ;; esac
case $initramfs in /*) ;; *) initramfs=$caller_dir/$initramfs ;; esac
test -f "$kernel"
test -f "$initramfs"

out=$(mktemp)
trap 'rm -f "$out"' EXIT
status=0
run_with_timeout 120 qemu-system-x86_64 \
    -m 512 -nographic -no-reboot \
    -machine hpet=off \
    -kernel "$kernel" \
    -initrd "$initramfs" \
    -append "console=ttyS0 panic=1 random.trust_cpu=off" \
    </dev/null >"$out" 2>&1 || status=$?
if [ "$status" -eq 124 ] || ! grep -q 'Kernel panic - not syncing: Harmony pvclock registration failed' "$out" || grep -q 'GUEST_READY' "$out"; then
    echo "FAIL: guest did not reject the missing Harmony host clock" >&2
    tr -d '\r' <"$out" | tail -30 >&2
    exit 1
fi
echo "ok: missing Harmony clock stopped boot before /init (QEMU status $status)"
