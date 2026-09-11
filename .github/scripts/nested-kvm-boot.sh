#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
kernel_version=$(uname -r)
root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{bin,dev,proc,sys,tmp,modules}
cp "$(command -v busybox)" "$root/bin/busybox"
for applet in sh mount insmod uname cat poweroff sleep; do
  ln -s busybox "$root/bin/$applet"
done
cc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -O2 -static \
  .github/scripts/nested-kvm-hlt.c -o "$root/bin/nested-kvm-hlt"
cp .github/scripts/nested-kvm-init.sh "$root/init"
chmod 755 "$root/init"
if test -d /sys/module/kvm_amd; then driver=kvm_amd; else driver=kvm_intel; fi
modprobe --show-depends --set-version "$kernel_version" "$driver" > reports/nested-module-deps.txt
: > "$root/modules.txt"
while read -r action source rest; do
  test "$action" = insmod || continue
  name=$(basename "$source")
  case "$source" in
    *.zst) name=${name%.zst}; zstd -q -d -c "$source" > "$root/modules/$name" ;;
    *.xz) name=${name%.xz}; xz -d -c "$source" > "$root/modules/$name" ;;
    *.ko) cp "$source" "$root/modules/$name" ;;
    *) printf 'Unsupported module compression: %s\n' "$source" >&2; exit 1 ;;
  esac
  printf '/modules/%s\n' "$name" >> "$root/modules.txt"
done < reports/nested-module-deps.txt
test -s "$root/modules.txt"
python3 .github/scripts/nested-kvm-initramfs.py "$root" reports/nested-initramfs.gz
sha256sum "$root/bin/nested-kvm-hlt" reports/nested-initramfs.gz > reports/nested-image-sha256.txt
sudo sha256sum "/boot/vmlinuz-$kernel_version" > reports/nested-kernel-sha256.txt
status=0
sudo timeout --signal=TERM --kill-after=5s 120s qemu-system-x86_64 \
  -machine pc,accel=kvm -cpu host -m 512M -smp 1 -nodefaults \
  -display none -serial stdio -monitor none -nic none -no-reboot \
  -kernel "/boot/vmlinuz-$kernel_version" -initrd reports/nested-initramfs.gz \
  -append 'console=ttyS0 panic=1 rdinit=/init' \
  2>&1 | tee reports/nested-console.raw || status=$?
printf '%s\n' "$status" > reports/nested-qemu-status.txt
tr -d '\r' < reports/nested-console.raw > reports/nested-console.txt
test "$status" -eq 0
test "$(grep -cx 'INNER_KVM_HLT=pass' reports/nested-console.txt)" -eq 1
grep -Fx 'INNER_PROBE_STATUS=0' reports/nested-console.txt
