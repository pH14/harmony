#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
kernel_version=$(uname -r)
kernel_image=${1:-/boot/vmlinuz-$kernel_version}
command_line="console=ttyS0 panic=1 rdinit=/init"
guest_memory=512M
root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
mkdir -p "$root"/{bin,dev,proc,sys,tmp,modules}
cp "$(command -v busybox)" "$root/bin/busybox"
for applet in sh mount insmod uname cat poweroff sleep mkdir grep tar gzip base64 wc; do
  "$root/bin/busybox" --list | grep -Fx "$applet" > /dev/null
  ln -s busybox "$root/bin/$applet"
done
cc -std=c11 -D_GNU_SOURCE -Wall -Wextra -Werror -O2 -static \
  .github/scripts/nested-kvm-hlt.c -o "$root/bin/nested-kvm-hlt"
cp .github/scripts/nested-kvm-init.sh "$root/init"
chmod 755 "$root/init"
if test $# -gt 0; then
  test $# -eq 1
  test "$kernel_image" = reports/nested-custom-bzImage
  test -s "$kernel_image"
  : > "$root/modules.txt"
  : > "$root/expected-sync-shadow"
  command_line="$command_line kvm.harmony_sync_shadow=1 kvm_amd.npt=0 kvm_intel.ept=0"
else
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
fi
if test "${NESTED_SNAPSHOT_TESTS:-false}" = true; then
  test "$kernel_image" = reports/nested-custom-bzImage
  bash .github/scripts/nested-kvm-stage-tests.sh "$root"
  : > "$root/expected-snapshot-tests"
  # Raw captures for six cases exceed the 256 MiB rootfs limit of a 512 MiB
  # guest. Retain them without changing the tested 4 MiB inner VMs.
  guest_memory=2048M
fi
python3 .github/scripts/nested-kvm-initramfs.py "$root" reports/nested-initramfs.gz
sha256sum "$root/bin/nested-kvm-hlt" reports/nested-initramfs.gz > reports/nested-image-sha256.txt
sudo sha256sum "$kernel_image" > reports/nested-kernel-sha256.txt
printf '%s\n' "$guest_memory" > reports/nested-guest-memory.txt
status=0
sudo timeout --signal=TERM --kill-after=5s 120s qemu-system-x86_64 \
  -machine pc,accel=kvm -cpu host -m "$guest_memory" -smp 1 -nodefaults \
  -display none -serial stdio -monitor none -nic none -no-reboot \
  -kernel "$kernel_image" -initrd reports/nested-initramfs.gz \
  -append "$command_line" \
  2>&1 | tee reports/nested-console.raw || status=$?
printf '%s\n' "$status" > reports/nested-qemu-status.txt
tr -d '\r' < reports/nested-console.raw > reports/nested-console.txt
if test "${NESTED_SNAPSHOT_TESTS:-false}" = true; then
  python3 .github/scripts/nested-kvm-extract-evidence.py \
    reports/nested-console.txt reports/nested-snapshot-evidence
  grep -Fx 'INNER_SNAPSHOT_TESTS_STATUS=0' reports/nested-console.txt
fi
test "$status" -eq 0
test "$(grep -cx 'INNER_KVM_HLT=pass' reports/nested-console.txt)" -eq 1
grep -Fx 'INNER_PROBE_STATUS=0' reports/nested-console.txt
