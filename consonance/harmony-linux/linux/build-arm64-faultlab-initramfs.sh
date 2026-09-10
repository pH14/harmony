#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the ARM64 fault-library base initramfs. workloads/faults appends its
# package-owned /init and OCI cpio members to this archive, so this image ships
# only the static BusyBox shell/tool surface and the kernel device nodes that
# PID 1 and that init require.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make gzip python3 readelf objdump
extract_busybox
extract_kernel # for usr/gen_init_cpio.c
prepare_busybox_build_source

mkdir -p "$ARM64_ART_DIR"
busybox_obj=$BUILD_ROOT/busybox-build-arm64-faultlab
faultlab_root=$BUILD_ROOT/arm64-faultlab-root

# Build with the owned ARM musl runtime so the base image is static and uses
# the same LSE-only userspace contract as the other ARM workload images. The
# locked builder normally built this prefix for the other ARM payloads; the
# standalone Make target must be able to prepare it as well.
if [ ! -x "$ARM64_GAME_MUSL_PREFIX/bin/musl-gcc" ] || \
    [ ! -f "$ARM64_GAME_MUSL_PREFIX/lib/libc.a" ]; then
    build_arm64_game_musl
fi
musl_prefix=$ARM64_GAME_MUSL_PREFIX
musl_cc=$musl_prefix/bin/musl-gcc

echo "== arm64 faultlab initramfs: building static BusyBox ($BUSYBOX_VERSION)"
rm -rf "$busybox_obj" "$faultlab_root"
mkdir -p "$faultlab_root"/bin "$faultlab_root"/dev "$faultlab_root"/proc \
    "$faultlab_root"/sys "$faultlab_root"/run "$faultlab_root"/tmp
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

# This is the complete external applet surface of prepare.rs::INIT: mkdir,
# mount, grep, ip link, chmod, chroot, and the shell's echo support. Check the
# package source here so an init change cannot silently leave this base image
# with an applet missing from the generated package init.
fault_init=$GUEST_DIR/../../workloads/faults/src/prepare.rs
[ -f "$fault_init" ] || {
    echo "FAIL: workloads/faults prepare source is missing: $fault_init" >&2
    exit 1
}
for applet in mkdir grep mount ip chmod; do
    grep -Eq "^[[:space:]]*(if[[:space:]]+![[:space:]]+)?\\\$BB[[:space:]]+$applet([[:space:]]|$)" "$fault_init" || {
        echo "FAIL: faultlab BusyBox contract lost prepare init applet: $applet" >&2
        exit 1
    }
done
# shellcheck disable=SC2016 # the dollar sign is a literal source-contract marker
grep -Eq '^[[:space:]]*exec[[:space:]]+\$BB[[:space:]]+chroot[[:space:]]' "$fault_init" || {
    echo "FAIL: faultlab BusyBox contract lost prepare init applet: chroot" >&2
    exit 1
}
# BusyBox's `mount --bind` long option is implemented by parsing the `bind`
# action flag from the FEATURE_MOUNT_FLAGS table; MOUNT alone only accepts
# ro/rw/remount. Keep this enabled even though the init does not use -o flags.
symbols='STATIC BUSYBOX ASH SH_IS_ASH MOUNT FEATURE_MOUNT_FLAGS MKDIR CHMOD CHROOT ECHO GREP IP FEATURE_IP_ADDRESS FEATURE_IP_LINK'
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
        echo "FAIL: arm64 faultlab BusyBox lost CONFIG_${symbol}" >&2
        exit 1
    }
done
grep -qxF 'CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"' \
    "$busybox_obj/.config" || {
    echo "FAIL: arm64 faultlab BusyBox lost its LSE-only compiler flags" >&2
    exit 1
}
make -C "$BBSRC" O="$busybox_obj" CC="$musl_cc" -j"$(nproc)" busybox >/dev/null

expected_applet_table=$(printf '%s\n' \
    'const char applet_names[] ALIGN1 = ""' \
    '"ash" "\0"' \
    '"chmod" "\0"' \
    '"chroot" "\0"' \
    '"echo" "\0"' \
    '"grep" "\0"' \
    '"ip" "\0"' \
    '"mkdir" "\0"' \
    '"mount" "\0"' \
    '"sh" "\0"' \
    ';')
actual_applet_table=$(sed -n '/^const char applet_names/,/^;$/p' \
    "$busybox_obj/include/applet_tables.h")
if [ "$actual_applet_table" != "$expected_applet_table" ]; then
    echo "FAIL: arm64 faultlab BusyBox applet surface changed" >&2
    echo "expected:" >&2
    printf '%s\n' "$expected_applet_table" >&2
    echo "actual:" >&2
    printf '%s\n' "$actual_applet_table" >&2
    exit 1
fi
if [ "$("$busybox_obj/busybox" echo dispatcher-ok)" != dispatcher-ok ]; then
    echo "FAIL: arm64 faultlab BusyBox dispatcher cannot invoke an applet" >&2
    exit 1
fi
if ! "$busybox_obj/busybox" ip link help >/dev/null 2>&1; then
    echo "FAIL: arm64 faultlab BusyBox ip lacks the link subcommand" >&2
    exit 1
fi

# Exercise long-option parsing without ever mounting anything: both operands
# are required to be absent, so a successful mount would be a build failure.
# A parser/configuration failure is distinguished from the expected kernel
# error, catching accidental removal of FEATURE_MOUNT_FLAGS without touching
# the builder's mount namespace.
mount_bind_source=$BUILD_ROOT/.harmony-faultlab-bind-source-must-not-exist
mount_bind_target=$BUILD_ROOT/.harmony-faultlab-bind-target-must-not-exist
mount_bind_log=$BUILD_ROOT/arm64-faultlab-mount-bind.log
if [ -e "$mount_bind_source" ] || [ -L "$mount_bind_source" ] || \
    [ -e "$mount_bind_target" ] || [ -L "$mount_bind_target" ]; then
    echo "FAIL: mount --bind smoke paths unexpectedly exist" >&2
    exit 1
fi
if "$busybox_obj/busybox" mount --bind "$mount_bind_source" \
    "$mount_bind_target" >"$mount_bind_log" 2>&1; then
    echo "FAIL: arm64 faultlab BusyBox mount --bind unexpectedly succeeded" >&2
    exit 1
fi
if grep -Eiq '(unknown|invalid|unrecognized)[[:space:]]+option|usage:' \
    "$mount_bind_log"; then
    echo "FAIL: arm64 faultlab BusyBox mount --bind was not parsed" >&2
    cat "$mount_bind_log" >&2
    exit 1
fi

install -m 0755 "$busybox_obj/busybox" "$faultlab_root/bin/busybox"
# The package init calls /bin/busybox directly. The /bin/sh link is useful to
# shell-form setup and hook commands when an OCI image inherits the base tree;
# workload-owned files arrive in the appended OCI segment.
ln -sf busybox "$faultlab_root/bin/sh"

# PID 1 opens the console before it can mount devtmpfs; /dev/null is used by
# the generated init's guarded probes. kmsg keeps early diagnostics available
# on boards whose tty console is intentionally absent.
cat >"$BUILD_ROOT/initramfs-arm64-faultlab.spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
nod /dev/kmsg 0600 0 0 c 1 11
nod /dev/null 0666 0 0 c 1 3
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /run 0755 0 0
dir /tmp 0777 0 0
dir /bin 0755 0 0
file /bin/busybox $faultlab_root/bin/busybox 0755 0 0
slink /bin/sh /bin/busybox 0777 0 0
EOF

cc -O2 -o "$BUILD_ROOT/gen_init_cpio-arm64-faultlab" "$KSRC/usr/gen_init_cpio.c"
"$BUILD_ROOT/gen_init_cpio-arm64-faultlab" -t 0 \
    "$BUILD_ROOT/initramfs-arm64-faultlab.spec" \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs-faultlab.cpio.gz"

# Shipped base userspace is subject to the same ARM executable gates as the
# kernel and other ARM workload support binaries.
python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$busybox_obj/busybox"
python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$busybox_obj/busybox"
echo "ok: $ARM64_ART_DIR/initramfs-faultlab.cpio.gz"
