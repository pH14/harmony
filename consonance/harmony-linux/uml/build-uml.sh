#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned User-mode Linux executable, its fixture initramfs, and the
# profile that names their hashes. The source tree is separate from the
# hardware-guest tree because the two carry different patch series.
set -euo pipefail

cd "$(dirname "$0")/../linux"

# shellcheck source=../linux/lib-build.sh disable=SC1091
. ./lib-build.sh

UML_DIR=$GUEST_DIR/uml
[ "$(uname -s)" = Linux ] || {
    echo "FAIL: User-mode Linux builds only on Linux" >&2
    exit 1
}
arch=$(uname -m)
case "$arch" in
    x86_64) ;;
    *)
        echo "FAIL: no User-mode Linux profile for $arch" >&2
        exit 1
        ;;
esac
require_tools cc make flex bison bc xz gzip readelf python3

uml_root=$BUILD_ROOT/um-$arch
uml_src=$uml_root/linux-$KERNEL_VERSION
uml_obj=$uml_root/build
uml_out=$ART_DIR/uml/$arch

tarball=$DL_DIR/$(basename "$KERNEL_URL")
[ -f "$tarball" ] || {
    echo "FAIL: $tarball missing; run 'make -C consonance/harmony-linux fetch' first" >&2
    exit 1
}
[ "$(sha256_of "$tarball")" = "$KERNEL_SHA256" ] || {
    echo "FAIL: $tarball sha256 mismatch" >&2
    exit 1
}
if [ ! -d "$uml_src" ]; then
    mkdir -p "$uml_root"
    tar -xf "$tarball" -C "$uml_root"
fi
bash "$LINUX_DIR/apply-patch-series.sh" "$uml_src" \
    "$LINUX_DIR/patches/common" "$LINUX_DIR/patches/um"

kmake() {
    make -C "$uml_src" O="$uml_obj" ARCH=um SUBARCH="$arch" LOCALVERSION= "$@"
}

echo "== uml: defconfig + overlay (linux-$KERNEL_VERSION, $arch)"
mkdir -p "$uml_obj"
kmake defconfig
(cd "$uml_src" && ./scripts/kconfig/merge_config.sh -m -O "$uml_obj" \
    "$uml_obj/.config" "$UML_DIR/config-fragment")
kmake olddefconfig

assert_y() {
    for sym in "$@"; do
        grep -qxF "CONFIG_$sym=y" "$uml_obj/.config" || {
            echo "FAIL: CONFIG_$sym=y did not survive merge_config/olddefconfig" >&2
            exit 1
        }
    done
}
assert_off() {
    for sym in "$@"; do
        if grep -q "^CONFIG_$sym=" "$uml_obj/.config"; then
            echo "FAIL: CONFIG_$sym is enabled but must be off" >&2
            exit 1
        fi
    done
}
assert_y STATIC_LINK UML_TIME_TRAVEL_SUPPORT HZ_PERIODIC BLK_DEV_INITRD \
    RD_GZIP DEVTMPFS BINFMT_ELF PROC_FS TMPFS FUTEX SECCOMP_FILTER NULL_CHAN \
    HARMONY_DEVICE
assert_off SMP MODULES NO_HZ_COMMON HIGH_RES_TIMERS LOCALVERSION_AUTO HOSTFS \
    UML_RANDOM HW_RANDOM MCONSOLE BLK_DEV_UBD UML_NET_VECTOR MAY_HAVE_RUNTIME_DEPS \
    PORT_CHAN PTY_CHAN TTY_CHAN XTERM_CHAN UML_RTC VIRTIO_UML UML_PCI
grep -qxF 'CONFIG_HZ=100' "$uml_obj/.config" || {
    echo "FAIL: CONFIG_HZ must be 100" >&2
    exit 1
}
grep -qxF 'CONFIG_LOCALVERSION=""' "$uml_obj/.config" || {
    echo "FAIL: CONFIG_LOCALVERSION must be empty" >&2
    exit 1
}

echo "== uml: building linux"
kmake -j"$(nproc)" linux

linux_headers=$(readelf -h -l -d "$uml_obj/linux")
if grep -Eq ' INTERP |\(NEEDED\)' <<<"$linux_headers"; then
    echo "FAIL: the User-mode Linux executable must be statically linked" >&2
    exit 1
fi

echo "== uml: fixture initramfs"
build_x86_musl
"$X86_MUSL_PREFIX/bin/musl-gcc" -O2 -static -Wall -Wextra -Werror \
    -o "$uml_root/fixture-init" "$UML_DIR/fixture-init.c"
"$X86_MUSL_PREFIX/bin/musl-gcc" -O2 -static -nostdlib -ffreestanding -fno-builtin \
    -fno-stack-protector -fno-tree-loop-distribute-patterns -Wall -Wextra -Werror \
    -o "$uml_root/fixture-registers" "$UML_DIR/fixture-registers.c"
cc -O2 -o "$uml_root/gen_init_cpio" "$uml_src/usr/gen_init_cpio.c"
spec=$uml_root/initramfs.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
file /init $uml_root/fixture-init 0755 0 0
file /registers $uml_root/fixture-registers 0755 0 0
EOF
mkdir -p "$uml_out"
"$uml_root/gen_init_cpio" -t 0 "$spec" | gzip -n -9 >"$uml_out/initramfs.cpio.gz.tmp"

install -m 0755 "$uml_obj/linux" "$uml_out/linux"
install -m 0644 "$uml_obj/.config" "$uml_out/config"
mv "$uml_out/initramfs.cpio.gz.tmp" "$uml_out/initramfs.cpio.gz"
python3 "$UML_DIR/write-profile.py" --output "$uml_out" --architecture "$arch" \
    --kernel-version "$KERNEL_VERSION" --patch-series "$uml_src/.harmony-patch-series"
echo "ok: $uml_out"
