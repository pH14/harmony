#!/bin/bash
# nested-x86 N-1: assemble the consonance appliance initramfs on the box.
#
# Contents: busybox + glibc runtime + the vmm-core live-gate test binaries +
# the PATCHED 6.12.90 kvm modules (deb612 build) + stock deps + the pinned
# L2 postgres pair + the C1 payload ELFs, laid out under /root/harmony-nested
# exactly where the compile-time CARGO_MANIFEST_DIR paths expect them.
# The L1 kernel is the box's own /boot/vmlinuz (identical binary, so the
# patched modules' vermagic matches inside L1 by construction).
#
# Usage: build-appliance.sh <gate-binary> [<gate-binary> ...]
set -euo pipefail

BASE=/root/nested-x86-spike/n1
KVER=6.12.90+deb13.1-amd64
PATCHED=/root/kvm-spike/deb612/hdr/usr/src/linux-headers-$KVER/arch/x86/kvm
SRCROOT=/root/harmony-nested
PG=/root/harmony-pr44/guest/build
IR=$BASE/initramfs

# pinned L2 postgres pair (pr44 build; foreman ruling hm-xdp)
PIN_BZIMAGE=f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41
PIN_INITRAMFS=3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2
# patched 6.12.90 kvm modules (verified against the deb612 tree at build time)
PIN_KVM_KO=ce998d6aeb1e9aa694368061e023d1db5e658333c117c405aed212462c543452
PIN_KVM_INTEL_KO=b6e6d3d2c4fd6f08a67ce00d39d9a735219625e5bca4e33a572ce943da13ed2e

[ $# -ge 1 ] || { echo "usage: $0 <gate-binary>..."; exit 2; }

rm -rf "$IR"
mkdir -p "$BASE" "$IR"/{bin,dev,proc,sys,tmp,mod,gate,lib/x86_64-linux-gnu,lib64}
mkdir -p "$IR/root/harmony-nested/guest/build"
mkdir -p "$IR/root/harmony-nested/guest/payloads/target/x86_64-unknown-none/release"
# tests resolve artifacts via CARGO_MANIFEST_DIR/../.. — the manifest dir chain
# must exist in the initramfs for `..` traversal to resolve
mkdir -p "$IR/root/harmony-nested/consonance/vmm-core"

pin() { # pin <file> <sha256>
    local got; got=$(sha256sum "$1" | cut -d' ' -f1)
    [ "$got" = "$2" ] || { echo "PIN MISMATCH $1: got $got want $2"; exit 1; }
}

# busybox + applets
BB=/usr/bin/busybox
file $BB | grep -q "statically linked"
cp $BB "$IR/bin/busybox"
for app in sh mount insmod rmmod dmesg poweroff ls cat grep sleep mknod uname date sha256sum mkdir cp df free; do
    ln -sf busybox "$IR/bin/$app"
done

# gate binaries + their dynamic library closure
for GATE in "$@"; do
    name=$(basename "$GATE" | sed 's/-[0-9a-f]*$//')
    cp "$GATE" "$IR/gate/$name"
    chmod +x "$IR/gate/$name"
    for lib in $(ldd "$GATE" | grep -o '/[^ ]*' | sort -u); do
        d="$IR${lib%/*}"
        mkdir -p "$d"
        cp -n "$lib" "$d/" 2>/dev/null || true
    done
done

# modules: stock deps + PATCHED kvm/kvm-intel (NOT the stock ones)
for m in msr irqbypass; do
    p=$(modinfo -n "$m")
    case "$p" in
        *.xz) xz -dkc "$p" > "$IR/mod/$(basename "${p%.xz}")" ;;
        *)    cp "$p" "$IR/mod/" ;;
    esac
done
pin "$PATCHED/kvm.ko" "$PIN_KVM_KO"
pin "$PATCHED/kvm-intel.ko" "$PIN_KVM_INTEL_KO"
cp "$PATCHED/kvm.ko" "$IR/mod/kvm.ko"
cp "$PATCHED/kvm-intel.ko" "$IR/mod/kvm-intel.ko"

# pinned L2 postgres pair, at the exact compile-time repo_root() path
pin "$PG/bzImage" "$PIN_BZIMAGE"
pin "$PG/initramfs-postgres.cpio.gz" "$PIN_INITRAMFS"
cp "$PG/bzImage" "$IR/root/harmony-nested/guest/build/"
cp "$PG/initramfs-postgres.cpio.gz" "$IR/root/harmony-nested/guest/build/"

# C1 payload ELFs (live_preemption + box_corpus), at their compile-time path
find "$SRCROOT/guest/payloads/target/x86_64-unknown-none/release" -maxdepth 1 -type f -executable \
    -exec cp {} "$IR/root/harmony-nested/guest/payloads/target/x86_64-unknown-none/release/" \;

# corpus manifest + committed goldens (box_corpus O2)
mkdir -p "$IR/root/harmony-nested/docs" "$IR/root/harmony-nested/guest/golden"
cp "$SRCROOT/docs/corpus-manifest.toml" "$IR/root/harmony-nested/docs/"
cp -r "$SRCROOT/guest/golden/." "$IR/root/harmony-nested/guest/golden/"

cp /root/nested-x86-spike/n1/src/l1-appliance-init.sh "$IR/init"
chmod +x "$IR/init"
[ -e "$IR/dev/console" ] || mknod "$IR/dev/console" c 5 1

( cd "$IR" && find . | cpio -o -H newc --quiet | gzip -1 ) > "$BASE/appliance.cpio.gz"

# build manifest with content hashes
{
  echo "{"
  echo "  \"kver\": \"$KVER\","
  echo "  \"source\": \"spike/nested-x86 rsync of $(cat $SRCROOT/.spike-source-commit 2>/dev/null || echo unknown)\","
  echo "  \"gates\": ["
  first=1
  for GATE in "$@"; do
    [ $first = 1 ] || echo ","
    first=0
    printf '    {"name": "%s", "sha256": "%s"}' "$(basename "$GATE")" "$(sha256sum "$GATE" | cut -d' ' -f1)"
  done
  echo ""
  echo "  ],"
  echo "  \"sha256_kvm_ko_patched\": \"$PIN_KVM_KO\","
  echo "  \"sha256_kvm_intel_ko_patched\": \"$PIN_KVM_INTEL_KO\","
  echo "  \"sha256_l2_bzImage\": \"$PIN_BZIMAGE\","
  echo "  \"sha256_l2_initramfs_postgres\": \"$PIN_INITRAMFS\","
  echo "  \"sha256_appliance_cpio\": \"$(sha256sum "$BASE/appliance.cpio.gz" | cut -d' ' -f1)\","
  echo "  \"sha256_l1_kernel\": \"$(sha256sum /boot/vmlinuz-$KVER | cut -d' ' -f1)\","
  echo "  \"built\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$BASE/build-manifest.json"

echo BUILD_OK
cat "$BASE/build-manifest.json"
