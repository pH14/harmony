#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the arm64 OCI-runner initramfs: static LSE-only busybox and nothing
# else. It ships no /init — the harmony CLI appends a bundle segment (rootfs +
# config + init) and selects it with rdinit=/harmony-oci-init, so this image
# only has to supply the shell and mount/chroot tooling that init uses.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make gzip cpio python3 readelf patch
extract_busybox
prepare_busybox_build_source

oci_root=$BUILD_ROOT/arm64-oci-root
busybox_obj=$BUILD_ROOT/busybox-build-arm64-oci

echo "== arm64 oci image: building LSE-only static musl ($MUSL_VERSION)"
build_arm64_game_musl
musl_cc=$ARM64_GAME_MUSL_PREFIX/bin/musl-gcc

echo "== arm64 oci image: building LSE-only static busybox ($BUSYBOX_VERSION)"
rm -rf "$busybox_obj"
mkdir -p "$busybox_obj" "$ARM64_ART_DIR"
make -C "$BBSRC" O="$busybox_obj" allnoconfig >/dev/null

enable_busybox_symbol() {
    local symbol=$1

    if grep -qxF "CONFIG_${symbol}=y" "$busybox_obj/.config"; then
        return
    fi
    grep -qxF "# CONFIG_${symbol} is not set" "$busybox_obj/.config" || {
        echo "FAIL: BusyBox has no disabled CONFIG_${symbol} setting" >&2
        exit 1
    }
    sed "s/^# CONFIG_${symbol} is not set$/CONFIG_${symbol}=y/" \
        "$busybox_obj/.config" >"$busybox_obj/.config.tmp"
    mv "$busybox_obj/.config.tmp" "$busybox_obj/.config"
}

# The game image's applet surface plus what the injected oci init needs:
# mkdir (mountpoints), chroot (the no-runc container start), poweroff.
symbols='STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKDIR MKNOD CHMOD CHROOT CAT ECHO GREP HALT POWEROFF REBOOT'
for symbol in $symbols; do
    enable_busybox_symbol "$symbol"
done
grep -qxF 'CONFIG_EXTRA_CFLAGS=""' "$busybox_obj/.config" || {
    echo "FAIL: BusyBox default compiler flags changed" >&2
    exit 1
}
sed 's/^CONFIG_EXTRA_CFLAGS=""$/CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"/' \
    "$busybox_obj/.config" >"$busybox_obj/.config.tmp"
mv "$busybox_obj/.config.tmp" "$busybox_obj/.config"
set +o pipefail
yes '' | make -C "$BBSRC" O="$busybox_obj" oldconfig >/dev/null
set -o pipefail
for symbol in $symbols; do
    grep -qxF "CONFIG_${symbol}=y" "$busybox_obj/.config" || {
        echo "FAIL: arm64 oci BusyBox lost CONFIG_${symbol}" >&2
        exit 1
    }
done
grep -qxF 'CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"' \
    "$busybox_obj/.config" || {
    echo "FAIL: arm64 oci busybox lost its LSE-only compiler flags" >&2
    exit 1
}
make -C "$BBSRC" O="$busybox_obj" CC="$musl_cc" -j"$(nproc)" busybox >/dev/null
expected_applet_table=$(printf '%s\n' \
    'const char applet_names[] ALIGN1 = ""' \
    '"ash" "\0"' \
    '"cat" "\0"' \
    '"chmod" "\0"' \
    '"chroot" "\0"' \
    '"echo" "\0"' \
    '"grep" "\0"' \
    '"halt" "\0"' \
    '"mkdir" "\0"' \
    '"mknod" "\0"' \
    '"mount" "\0"' \
    '"poweroff" "\0"' \
    '"reboot" "\0"' \
    '"sh" "\0"' \
    ';')
actual_applet_table=$(sed -n '/^const char applet_names/,/^;$/p' \
    "$busybox_obj/include/applet_tables.h")
if [ "$actual_applet_table" != "$expected_applet_table" ]; then
    echo "FAIL: arm64 oci BusyBox applet surface changed" >&2
    echo "expected:" >&2
    printf '%s\n' "$expected_applet_table" >&2
    echo "actual:" >&2
    printf '%s\n' "$actual_applet_table" >&2
    exit 1
fi
if [ "$("$busybox_obj/busybox" echo dispatcher-ok)" != dispatcher-ok ]; then
    echo "FAIL: arm64 oci BusyBox dispatcher cannot invoke an applet" >&2
    exit 1
fi

echo "== arm64 oci image: assembling rootfs"
rm -rf "$oci_root"
mkdir -p "$oci_root"/{bin,etc,proc,sys,dev,run,tmp}
install -m 0755 "$busybox_obj/busybox" "$oci_root/bin/busybox"
for applet in sh mount mkdir mknod chmod chroot cat echo grep halt poweroff reboot; do
    ln -sf busybox "$oci_root/bin/$applet"
done
printf 'root:x:0:0:root:/root:/bin/sh\n' >"$oci_root/etc/passwd"
printf 'root:x:0:\n' >"$oci_root/etc/group"

# The shipped userspace (busybox only) stays inside the LSE-only / no-live-
# counter contract; the injected container rootfs is the user's and is outside
# it by definition (DETERMINISM.md trust section).
echo "== arm64 oci image: scanning every executable mapping"
while read -r binary; do
    if readelf -h "$binary" >/dev/null 2>&1; then
        python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$binary"
        python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$binary"
    fi
done < <(find "$oci_root" \( -type f -perm -0100 -o -type f -name '*.so*' \) | LC_ALL=C sort)

echo "== arm64 oci image: packing reproducibly"
find "$oci_root" -mindepth 1 -exec touch -hcd @0 {} +
(cd "$oci_root" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --reproducible --quiet) \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs-oci.cpio.gz"
sha=$(sha256_of "$ARM64_ART_DIR/initramfs-oci.cpio.gz")
printf '%s  initramfs-oci.cpio.gz\n' "$sha" >"$ARM64_ART_DIR/initramfs-oci.cpio.gz.sha256"
echo "ok: $ARM64_ART_DIR/initramfs-oci.cpio.gz"
