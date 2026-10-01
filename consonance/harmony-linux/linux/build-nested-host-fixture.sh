#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
output=${1:?usage: build-nested-host-fixture.sh OUTPUT}
mkdir -p "$output"
output=$(cd "$output" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tree/bin" "$work/tree/proc" "$work/tree/dev" "$work/tree/sys"
cc -O2 -static -Wall -Wextra -Werror "$here/nested-kvm-check.c" -o "$work/tree/check"
install -m 0755 "${NESTED_HOST_BUSYBOX:?provide a static BusyBox}" "$work/tree/bin/busybox"
cat >"$work/tree/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
timer=$(/bin/busybox cat /sys/module/kvm_intel/parameters/preemption_timer)
echo "NESTED_PREEMPTION_TIMER=$timer"
if [ "$timer" != N ]; then
    echo "FAIL: nested-host hardware preemption timer is enabled"
    /bin/busybox poweroff -f
    exit 1
fi
/check
status=$?
echo "NESTED_CHECK_STATUS=$status"
/bin/busybox poweroff -f
INIT
chmod 0755 "$work/tree/init"
find "$work/tree" -exec touch -h -d @0 {} +
(cd "$work/tree" && find . -print0 | LC_ALL=C sort -z | \
    cpio --null -o --format=newc --owner=0:0 --reproducible --quiet) | gzip -n -9 >"$output/initramfs-nested-host.cpio.gz"
cp "$work/tree/check" "$output/nested-kvm-check"
