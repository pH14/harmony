#!/bin/bash
# nested-x86 N-1: assemble the consonance appliance initramfs on the box.
#
# Contents: busybox + glibc runtime + the vmm-core live-gate test binary +
# the PATCHED 6.12.90 kvm modules (deb612 Part-2 build) + stock deps.
# The L1 kernel is the box's own /boot/vmlinuz (identical binary, so the
# patched modules' vermagic/CRCs match inside L1 by construction).
#
# Usage: build-appliance.sh <gate-binary-path> [extra-artifact-dir]
set -euo pipefail

BASE=/root/nested-x86-spike/n1
KVER=6.12.90+deb13.1-amd64
PATCHED=/root/kvm-spike/deb612/hdr/usr/src/linux-headers-$KVER/arch/x86/kvm
GATE="${1:?path to gate test binary}"
IR=$BASE/initramfs

rm -rf "$IR"
mkdir -p "$BASE" "$IR"/{bin,dev,proc,sys,tmp,mod,gate,lib/x86_64-linux-gnu,lib64}

# busybox + applets
BB=/usr/bin/busybox
file $BB | grep -q "statically linked"
cp $BB "$IR/bin/busybox"
for app in sh mount insmod rmmod dmesg poweroff ls cat grep sleep mknod uname date; do
    ln -sf busybox "$IR/bin/$app"
done

# gate binary + its dynamic library closure
cp "$GATE" "$IR/gate/live_determinism"
chmod +x "$IR/gate/live_determinism"
for lib in $(ldd "$GATE" | grep -o '/[^ ]*' | sort -u); do
    d="$IR${lib%/*}"
    mkdir -p "$d"
    cp "$lib" "$d/"
done

# modules: stock deps + PATCHED kvm/kvm-intel (NOT the stock ones)
for m in msr irqbypass; do
    p=$(modinfo -n "$m")
    case "$p" in
        *.xz) xz -dkc "$p" > "$IR/mod/$(basename "${p%.xz}")" ;;
        *)    cp "$p" "$