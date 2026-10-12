#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
profile=${1:?UML profile directory required}
host_kernel=${2:?x86_64 host Linux kernel required}
busybox=${3:?static BusyBox required}
evidence=${4:?evidence directory required}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root"/{bin,proc,sys,dev,tmp}
cp "$busybox" "$root/bin/busybox"
ln -s busybox "$root/bin/sh"
cp "$profile/linux" "$root/linux"
cp "$profile/initramfs.cpio.gz" "$root/fixture.gz"
cat > "$root/init" <<'INIT'
#!/bin/sh
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/linux mem=64M initrd=/fixture.gz seccomp=on time-travel=inf-cpu con0=fd:0,fd:1 con=null harmony_fixture=boot
/bin/busybox poweroff -f
INIT
chmod +x "$root/init" "$root/linux"
(cd "$root" && find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc 2>/dev/null | gzip -n) > "$evidence/host.cpio.gz"
for cpu in qemu64,-xsave max; do
    name=${cpu%%,*}
    timeout 60 qemu-system-x86_64 -nographic -m 512 -accel tcg -cpu "$cpu" \
        -kernel "$host_kernel" -initrd "$evidence/host.cpio.gz" \
        -append 'console=ttyS0 rdinit=/init nohz=off' > "$evidence/$name.log" 2>&1
    grep -q 'HARMONY_UML PASS release=' "$evidence/$name.log"
    if grep -qE 'Kernel panic|SECCOMP userspace requested but not functional' "$evidence/$name.log"; then
        echo "FAIL: nested UML on $cpu" >&2
        exit 1
    fi
    echo "PASS: nested UML on $cpu"
done
