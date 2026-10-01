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
fragments=("$UML_DIR/config-fragment")
case "$arch" in
    x86_64)
        subarch=x86_64
        source_version=$KERNEL_VERSION source_url=$KERNEL_URL source_sha256=$KERNEL_SHA256
        series=um fetch_target=fetch
        ;;
    aarch64)
        subarch=arm64
        source_version=$UML_ARM64_VERSION source_url=$UML_ARM64_URL source_sha256=$UML_ARM64_SHA256
        series=um-arm64 fetch_target=fetch-uml-arm64
        fragments+=("$UML_DIR/config-fragment-arm64")
        ;;
    *)
        echo "FAIL: no User-mode Linux profile for $arch" >&2
        exit 1
        ;;
esac
require_tools cc make flex bison bc xz gzip readelf python3

uml_root=$BUILD_ROOT/um-$arch
uml_src=$uml_root/linux-$source_version
uml_obj=$uml_root/build
uml_out=$ART_DIR/uml/$arch

tarball=$DL_DIR/$(basename "$source_url")
[ -f "$tarball" ] || {
    echo "FAIL: $tarball missing; run 'make -C consonance/harmony-linux $fetch_target' first" >&2
    exit 1
}
[ "$(sha256_of "$tarball")" = "$source_sha256" ] || {
    echo "FAIL: $tarball sha256 mismatch" >&2
    exit 1
}
if [ ! -d "$uml_src" ]; then
    rm -rf "$uml_src.extract"
    mkdir -p "$uml_src.extract"
    tar -xf "$tarball" -C "$uml_src.extract" --strip-components=1
    mv "$uml_src.extract" "$uml_src"
fi
bash "$LINUX_DIR/apply-patch-series.sh" "$uml_src" \
    "$LINUX_DIR/patches/common" "$LINUX_DIR/patches/$series"

kmake() {
    make -C "$uml_src" O="$uml_obj" ARCH=um SUBARCH="$subarch" LOCALVERSION= "$@"
}

echo "== uml: defconfig + overlay (linux-$source_version, $arch)"
mkdir -p "$uml_obj"
kmake defconfig
(cd "$uml_src" && ./scripts/kconfig/merge_config.sh -m -O "$uml_obj" \
    "$uml_obj/.config" "${fragments[@]}")
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
    RD_GZIP DEVTMPFS BINFMT_ELF PROC_FS PROC_CHILDREN SYSCTL POSIX_TIMERS TMPFS FUTEX \
    SECCOMP_FILTER NULL_CHAN HARMONY_DEVICE NAMESPACES UTS_NS IPC_NS PID_NS NET_NS NET \
    UNIX INET CGROUPS CGROUP_SCHED CGROUP_PIDS CGROUP_DEVICE CGROUP_FREEZER CGROUP_BPF \
    BPF_SYSCALL UNIX98_PTYS
[ "$arch" != aarch64 ] || assert_y PAGE_SIZE_4KB
assert_off SMP MODULES VMAP_STACK BPF_JIT NO_HZ_COMMON HIGH_RES_TIMERS LOCALVERSION_AUTO HOSTFS \
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
if [ "$arch" = aarch64 ]; then
    build_arm64_musl
    musl_gcc=$ARM64_MUSL_PREFIX/bin/musl-gcc
else
    build_x86_musl
    musl_gcc=$X86_MUSL_PREFIX/bin/musl-gcc
fi
"$musl_gcc" -O2 -static -Wall -Wextra -Werror \
    -o "$uml_root/fixture-init" "$UML_DIR/fixture-init.c"
"$musl_gcc" -O2 -static -nostdlib -ffreestanding -fno-builtin \
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
    --kernel-version "$source_version" --patch-series "$uml_src/.harmony-patch-series"
echo "ok: $uml_out"
