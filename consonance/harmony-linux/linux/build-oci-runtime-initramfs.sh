#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the workload-free platform OCI runtime initramfs for one native
# architecture. Kernel boot, PID 1, supervision, and OCI execution are
# explicit inputs; this script never selects an application image or launch
# mode from guest contents.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

runtime_arch=${1:?usage: build-oci-runtime-initramfs.sh x86_64|aarch64}
case "$runtime_arch" in
    x86_64)
        require_linux_amd64
        kernel_artifact=$X86_64_ART_DIR/bzImage
        ;;
    aarch64)
        require_linux_aarch64
        kernel_artifact=$AARCH64_ART_DIR/Image
        ;;
    *)
        echo "FAIL: unsupported runtime architecture: $runtime_arch" >&2
        exit 2
        ;;
esac
require_tools cc make gzip readelf python3 sed grep awk nproc

runtime_init=${HARMONY_RUNTIME_INIT:-${HARMONY_PLATFORM_INIT:-}}
runtime_supervisor=${HARMONY_RUNTIME_SUPERVISOR:-${HARMONY_PLATFORM_SUPERVISOR:-}}
: "${runtime_init:?set HARMONY_RUNTIME_INIT to the platform PID 1 script}"
: "${runtime_supervisor:?set HARMONY_RUNTIME_SUPERVISOR to the platform supervisor}"
[ -x "$runtime_init" ] || {
    echo "FAIL: platform PID 1 is not executable: $runtime_init" >&2
    exit 1
}
[ -x "$runtime_supervisor" ] || {
    echo "FAIL: platform supervisor is not executable: $runtime_supervisor" >&2
    exit 1
}
[ -f "$kernel_artifact" ] || {
    echo "FAIL: build the $runtime_arch platform kernel first: $kernel_artifact" >&2
    exit 1
}

extract_kernel
runc_binary=$(extract_runc "$runtime_arch")
verify_static_runc "$runc_binary" "$runtime_arch"
if [ "$runtime_arch" = aarch64 ]; then
    echo "== OCI runtime: qualifying pinned runc before assembly"
    python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$runc_binary"
    python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$runc_binary"
fi

if [ "$runtime_arch" = aarch64 ]; then
    build_arm64_musl
    busybox_cc=$ARM64_MUSL_PREFIX/bin/musl-gcc
    busybox_cflags='-march=armv8.1-a+lse -mno-outline-atomics'
    busybox_flag_args=(-march=armv8.1-a+lse -mno-outline-atomics)
else
    busybox_cc=cc
    busybox_cflags=
    busybox_flag_args=()
fi

busybox_obj=$BUILD_ROOT/busybox-build-oci-$runtime_arch
oci_root=$BUILD_ROOT/oci-runtime-root-$runtime_arch
rm -rf "$busybox_obj" "$oci_root"
mkdir -p "$busybox_obj" "$oci_root"
prepare_busybox_build_source
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

# This is the complete command surface the platform init and supervisor may
# use. It deliberately excludes image-specific tools and namespace launchers.
for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT UMOUNT MKDIR MKNOD CHMOD CHOWN \
    CAT ECHO GREP HALT POWEROFF REBOOT SETSID SETUIDGID ENV ID KILL SLEEP \
    LN RM CP MV TRUE FALSE TEST SYNC PRINTF HEAD TAIL TEE CUT WC PS SED TOUCH \
    STAT READLINK; do
    enable_busybox_symbol "$symbol"
done
grep -qxF 'CONFIG_STATIC=y' "$busybox_obj/.config" || {
    echo "FAIL: platform BusyBox is not static" >&2
    exit 1
}
if [ -n "$busybox_cflags" ]; then
    grep -qxF 'CONFIG_EXTRA_CFLAGS=""' "$busybox_obj/.config" || {
        echo "FAIL: BusyBox default compiler flags changed" >&2
        exit 1
    }
    sed "s/^CONFIG_EXTRA_CFLAGS=\"\"$/CONFIG_EXTRA_CFLAGS=\"$busybox_cflags\"/" \
        "$busybox_obj/.config" >"$busybox_obj/.config.tmp"
    mv "$busybox_obj/.config.tmp" "$busybox_obj/.config"
fi
set +o pipefail
yes '' | make -C "$BBSRC" O="$busybox_obj" oldconfig >/dev/null
set -o pipefail
make -C "$BBSRC" O="$busybox_obj" CC="$busybox_cc" -j"$(nproc)" busybox >/dev/null
for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT UMOUNT MKDIR MKNOD CHMOD CHOWN \
    CAT ECHO GREP HALT POWEROFF REBOOT SETSID SETUIDGID ENV ID KILL SLEEP \
    LN RM CP MV TRUE FALSE TEST SYNC PRINTF HEAD TAIL TEE CUT WC PS SED TOUCH \
    STAT READLINK; do
    grep -qxF "CONFIG_${symbol}=y" "$busybox_obj/.config" || {
        echo "FAIL: platform BusyBox lost CONFIG_${symbol}" >&2
        exit 1
    }
done
if [ -n "$busybox_cflags" ]; then
    grep -qxF "CONFIG_EXTRA_CFLAGS=\"$busybox_cflags\"" "$busybox_obj/.config" || {
        echo "FAIL: arm64 BusyBox lost its LSE-only compiler flags" >&2
        exit 1
    }
fi
if grep -qxF 'CONFIG_CHROOT=y' "$busybox_obj/.config"; then
    echo "FAIL: platform BusyBox contains an image root transition applet" >&2
    exit 1
fi
if readelf -l "$busybox_obj/busybox" | grep -q ' INTERP '; then
    echo "FAIL: platform BusyBox is dynamically linked" >&2
    exit 1
fi

verify_static_elf() {
    local binary=$1
    if readelf -l "$binary" | grep -q ' INTERP '; then
        echo "FAIL: platform runtime ELF is dynamically linked: $binary" >&2
        exit 1
    fi
}

mkdir -p "$oci_root"/{bin,etc,proc,sys,dev,run,tmp,usr,usr/bin,usr/lib,usr/lib/harmony}
install -m 0755 "$busybox_obj/busybox" "$oci_root/bin/busybox"
for applet in sh mount umount mkdir mknod chmod chown cat echo grep halt poweroff \
    reboot setsid setuidgid env id kill sleep ln rm cp mv true false test sync \
    printf head tail tee cut wc ps sed touch stat readlink; do
    ln -sf busybox "$oci_root/bin/$applet"
done
install -m 0755 "$runc_binary" "$oci_root/usr/bin/runc"
install -m 0755 "$runtime_init" "$oci_root/usr/lib/harmony/init"
install -m 0755 "$runtime_supervisor" "$oci_root/usr/lib/harmony/supervisor"
install -m 0755 "$LINUX_DIR/oci-init.sh" "$oci_root/init"
if [ "$runtime_arch" = aarch64 ]; then
    "$busybox_cc" -static -Os "${busybox_flag_args[@]}" -Wall -Wextra -Werror \
        "$LINUX_DIR/arm64-mmio-console.c" -o "$oci_root/usr/bin/mmio-console"
fi

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$oci_root/etc/passwd"
printf 'root:x:0:\n' >"$oci_root/etc/group"
printf 'passwd: files\ngroup: files\nhosts: files\n' >"$oci_root/etc/nsswitch.conf"
# The kernel opens these before PID 1 can mount devtmpfs; the misc devices are
# created by devtmpfs and explicitly exposed by the platform init.
cat >"$BUILD_ROOT/oci-runtime.spec" <<EOF
dir /bin 0755 0 0
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
nod /dev/null 0666 0 0 c 1 3
nod /dev/kmsg 0600 0 0 c 1 11
dir /etc 0755 0 0
dir /proc 0555 0 0
dir /run 0755 0 0
dir /sys 0555 0 0
dir /tmp 1777 0 0
dir /usr 0755 0 0
dir /usr/bin 0755 0 0
dir /usr/lib 0755 0 0
dir /usr/lib/harmony 0755 0 0
file /bin/busybox $oci_root/bin/busybox 0755 0 0
file /etc/group $oci_root/etc/group 0644 0 0
file /etc/nsswitch.conf $oci_root/etc/nsswitch.conf 0644 0 0
file /etc/passwd $oci_root/etc/passwd 0644 0 0
file /init $oci_root/init 0755 0 0
file /usr/bin/runc $oci_root/usr/bin/runc 0755 0 0
file /usr/lib/harmony/init $oci_root/usr/lib/harmony/init 0755 0 0
file /usr/lib/harmony/supervisor $oci_root/usr/lib/harmony/supervisor 0755 0 0
slink /bin/cat /bin/busybox 0777 0 0
slink /bin/chmod /bin/busybox 0777 0 0
slink /bin/chown /bin/busybox 0777 0 0
slink /bin/cp /bin/busybox 0777 0 0
slink /bin/echo /bin/busybox 0777 0 0
slink /bin/env /bin/busybox 0777 0 0
slink /bin/false /bin/busybox 0777 0 0
slink /bin/grep /bin/busybox 0777 0 0
slink /bin/halt /bin/busybox 0777 0 0
slink /bin/head /bin/busybox 0777 0 0
slink /bin/id /bin/busybox 0777 0 0
slink /bin/kill /bin/busybox 0777 0 0
slink /bin/ln /bin/busybox 0777 0 0
slink /bin/mkdir /bin/busybox 0777 0 0
slink /bin/mknod /bin/busybox 0777 0 0
slink /bin/mount /bin/busybox 0777 0 0
slink /bin/mv /bin/busybox 0777 0 0
slink /bin/ps /bin/busybox 0777 0 0
slink /bin/poweroff /bin/busybox 0777 0 0
slink /bin/printf /bin/busybox 0777 0 0
slink /bin/readlink /bin/busybox 0777 0 0
slink /bin/reboot /bin/busybox 0777 0 0
slink /bin/rm /bin/busybox 0777 0 0
slink /bin/sed /bin/busybox 0777 0 0
slink /bin/setuidgid /bin/busybox 0777 0 0
slink /bin/setsid /bin/busybox 0777 0 0
slink /bin/sh /bin/busybox 0777 0 0
slink /bin/sleep /bin/busybox 0777 0 0
slink /bin/stat /bin/busybox 0777 0 0
slink /bin/sync /bin/busybox 0777 0 0
slink /bin/tail /bin/busybox 0777 0 0
slink /bin/tee /bin/busybox 0777 0 0
slink /bin/test /bin/busybox 0777 0 0
slink /bin/touch /bin/busybox 0777 0 0
slink /bin/true /bin/busybox 0777 0 0
slink /bin/umount /bin/busybox 0777 0 0
slink /bin/wc /bin/busybox 0777 0 0
EOF
if [ "$runtime_arch" = aarch64 ]; then
    cat >>"$BUILD_ROOT/oci-runtime.spec" <<EOF
file /usr/bin/mmio-console $oci_root/usr/bin/mmio-console 0755 0 0
EOF
fi

init_cpio=$BUILD_ROOT/gen_init_cpio-oci-$runtime_arch
cc -O2 -o "$init_cpio" "$KSRC/usr/gen_init_cpio.c"
mkdir -p "$ART_DIR/$runtime_arch"
initramfs=$ART_DIR/$runtime_arch/initramfs-oci.cpio.gz
"$init_cpio" -t 0 "$BUILD_ROOT/oci-runtime.spec" | gzip -n -9 >"$initramfs"

echo "== OCI runtime: scanning every shipped executable ($runtime_arch)"
while IFS= read -r -d '' binary; do
    if readelf -h "$binary" >/dev/null 2>&1; then
        verify_static_elf "$binary"
        if [ "$runtime_arch" = aarch64 ]; then
            python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$binary"
            python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$binary"
        fi
    fi
done < <(find "$oci_root" -type f -perm -0100 -print0 | LC_ALL=C sort -z)

sha=$(sha256_of "$initramfs")
printf '%s  initramfs-oci.cpio.gz\n' "$sha" >"$initramfs.sha256"
{
    printf 'format=1\narchitecture=%s\nrunc_version=%s\n' "$runtime_arch" "$RUNC_VERSION"
    printf 'kernel_sha256=%s\n' "$(sha256_of "$kernel_artifact")"
    printf 'initramfs_sha256=%s\n' "$sha"
    printf 'runc_sha256=%s\n' "$(sha256_of "$runc_binary")"
    printf 'busybox_sha256=%s\n' "$(sha256_of "$busybox_obj/busybox")"
    printf 'init_sha256=%s\n' "$(sha256_of "$runtime_init")"
    printf 'supervisor_sha256=%s\n' "$(sha256_of "$runtime_supervisor")"
} >"$ART_DIR/$runtime_arch/oci-runtime.manifest"
echo "ok: $initramfs"
