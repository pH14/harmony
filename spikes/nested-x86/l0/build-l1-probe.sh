#!/bin/bash
# nested-x86 N-0: build the minimal L1 probe initramfs on the box.
# Produces /root/nested-x86-spike/n0/l1-probe.cpio.gz + build-manifest.json
set -euo pipefail

BASE=/root/nested-x86-spike/n0
KVER=6.12.90+deb13.1-amd64
SRC=$BASE/src
IR=$BASE/initramfs

mkdir -p "$BASE" "$IR"/{bin,dev,proc,sys,mod}

# 1. static probe binary
gcc -O2 -static -o "$BASE/probe" "$SRC/probe.c"
file "$BASE/probe" | grep -q "statically linked"

# 2. static busybox (Debian /usr/bin/busybox may be dynamic)
if file /usr/bin/busybox | grep -q "statically linked"; then
    BB=/usr/bin/busybox
elif [ -x /bin/busybox ] && file /bin/busybox | grep -q "statically linked"; then
    BB=/bin/busybox
else
    echo "NOTE: installing busybox-static (record in restore manifest packages_installed)"
    DEBIAN_FRONTEND=noninteractive apt-get install -y -q busybox-static >/dev/null
    BB=/bin/busybox
    file $BB | grep -q "statically linked"
fi

# 3. assemble initramfs: busybox, probe, stock modules (decompressed), init
cp "$BB" "$IR/bin/busybox"
for app in sh mount insmod dmesg poweroff ls cat sleep mknod grep; do
    ln -sf busybox "$IR/bin/$app"
done
cp "$BASE/probe" "$IR/probe"

# resolve module paths against the running kernel (same KVER as L1 boots)
for m in msr irqbypass kvm kvm_intel; do
    p=$(modinfo -n "$m")
    case "$p" in
        *.xz) xz -dkc "$p" > "$IR/mod/$(basename "${p%.xz}")" ;;
        *)    cp "$p" "$IR/mod/" ;;
    esac
done

cp "$SRC/l1-init.sh" "$IR/init"
chmod +x "$IR/init" "$IR/probe"

# console node must exist in the cpio for early output
[ -e "$IR/dev/console" ] || mknod "$IR/dev/console" c 5 1

( cd "$IR" && find . | cpio -o -H newc --quiet | gzip -9 ) > "$BASE/l1-probe.cpio.gz"

# 4. build manifest with content hashes
{
  echo "{"
  echo "  \"kver\": \"$KVER\","
  echo "  \"gcc\": \"$(gcc -dumpfullversion)\","
  echo "  \"busybox\": \"$BB\","
  for f in "$BASE/probe" "$BASE/l1-probe.cpio.gz" "/boot/vmlinuz-$KVER"; do
    echo "  \"sha256_$(basename "$f")\": \"$(sha256sum "$f" | cut -d' ' -f1)\","
  done
  echo "  \"built\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$BASE/build-manifest.json"

echo BUILD_OK
cat "$BASE/build-manifest.json"
