#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the M2 arm64 TetaNES initramfs natively on the Linux/aarch64 build host.
# The current validated host is msr1. Every shipped executable and shared object
# is scanned for live counters and LL/SC.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make gzip cpio cargo python3 readelf patch
extract_busybox
prepare_busybox_build_source

game_root=$BUILD_ROOT/arm64-game-root
busybox_obj=$BUILD_ROOT/busybox-build-arm64-game

echo "== arm64 game image: building LSE-only static musl ($MUSL_VERSION)"
build_arm64_game_musl
musl_cc=$ARM64_GAME_MUSL_PREFIX/bin/musl-gcc

echo "== arm64 game image: building LSE-only static busybox ($BUSYBOX_VERSION)"
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

# Keep the shipped command surface explicit. A broad BusyBox defconfig brings
# unrelated host-header-dependent applets into this deterministic guest image.
for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKNOD CHMOD CAT ECHO GREP HALT REBOOT; do
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
for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKNOD CHMOD CAT ECHO GREP HALT REBOOT; do
    grep -qxF "CONFIG_${symbol}=y" "$busybox_obj/.config" || {
        echo "FAIL: arm64 game BusyBox lost CONFIG_${symbol}" >&2
        exit 1
    }
done
grep -qxF 'CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"' \
    "$busybox_obj/.config" || {
    echo "FAIL: arm64 game busybox lost its LSE-only compiler flags" >&2
    exit 1
}
make -C "$BBSRC" O="$busybox_obj" CC="$musl_cc" -j"$(nproc)" busybox >/dev/null
expected_applet_table=$(printf '%s\n' \
    'const char applet_names[] ALIGN1 = ""' \
    '"ash" "\0"' \
    '"cat" "\0"' \
    '"chmod" "\0"' \
    '"echo" "\0"' \
    '"grep" "\0"' \
    '"halt" "\0"' \
    '"mknod" "\0"' \
    '"mount" "\0"' \
    '"reboot" "\0"' \
    '"sh" "\0"' \
    ';')
actual_applet_table=$(sed -n '/^const char applet_names/,/^;$/p' \
    "$busybox_obj/include/applet_tables.h")
if [ "$actual_applet_table" != "$expected_applet_table" ]; then
    echo "FAIL: arm64 game BusyBox applet surface changed" >&2
    echo "expected:" >&2
    printf '%s\n' "$expected_applet_table" >&2
    echo "actual:" >&2
    printf '%s\n' "$actual_applet_table" >&2
    exit 1
fi
if [ "$("$busybox_obj/busybox" echo dispatcher-ok)" != dispatcher-ok ]; then
    echo "FAIL: arm64 game BusyBox dispatcher cannot invoke an applet" >&2
    exit 1
fi

echo "== arm64 game image: building TetaNES agent"
if [ -n "${HARMONY_TETANES_AGENT:-}" ]; then
    agent=$HARMONY_TETANES_AGENT
    [ -x "$agent" ] || {
        echo "FAIL: HARMONY_TETANES_AGENT is not executable: $agent" >&2
        exit 1
    }
    echo "== arm64 game image: using Nix-built offline TetaNES agent"
else
    if ! agent_output=$(HARMONY_MUSL_PREFIX="$ARM64_GAME_MUSL_PREFIX" \
        bash "$GUEST_DIR/tetanes-agent/build.sh"); then
        printf '%s\n' "$agent_output" >&2
        exit 1
    fi
    printf '%s\n' "$agent_output"
    agent=$(printf '%s\n' "$agent_output" | tail -1)
fi
[ -x "$agent" ] || { echo "FAIL: TetaNES agent missing: $agent" >&2; exit 1; }

echo "== arm64 game image: assembling rootfs"
rm -rf "$game_root"
mkdir -p "$game_root"/{bin,etc,proc,sys,dev,opt/harmony}
install -m 0755 "$busybox_obj/busybox" "$game_root/bin/busybox"
for applet in sh mount mknod chmod cat echo grep halt reboot; do
    ln -sf busybox "$game_root/bin/$applet"
done
install -m 0755 "$agent" "$game_root/opt/harmony/harmony-tetanes-agent"
install -m 0755 "$agent" "$ARM64_ART_DIR/harmony-tetanes-agent"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$game_root/etc/passwd"
printf 'root:x:0:\n' >"$game_root/etc/group"

if [ -n "${HARMONY_SMB_ROM:-}" ]; then
    [ -f "$HARMONY_SMB_ROM" ] || {
        echo "FAIL: HARMONY_SMB_ROM=$HARMONY_SMB_ROM does not exist" >&2
        exit 1
    }
    install -m 0644 "$HARMONY_SMB_ROM" "$game_root/opt/harmony/smb.nes"
    rom_sha=$(sha256_of "$game_root/opt/harmony/smb.nes")
    printf '%s\n' "$rom_sha" >"$game_root/opt/harmony/smb.nes.sha256"
    echo "== arm64 game image: ROM installed (sha256 $rom_sha)"
else
    rm -f "$ARM64_ART_DIR/initramfs-game.rom.sha256"
    echo "== arm64 game image: BLOCKED/SKIP — HARMONY_SMB_ROM unset" >&2
    echo "   The image will report TETANES_GAME_SKIP; no live M2 criterion is green." >&2
fi
install -m 0755 "$LINUX_DIR/arm64-game-init.sh" "$game_root/init"

# The M1 determinism contract applies to the whole shipped userspace, including
# the system C runtime. A generic distro libc containing dormant LL/SC is a loud
# build failure; it is not waived because the build host advertises LSE.
echo "== arm64 game image: scanning every executable mapping"
while read -r binary; do
    if readelf -h "$binary" >/dev/null 2>&1; then
        python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$binary"
        python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$binary"
    fi
done < <(find "$game_root" \( -type f -perm -0100 -o -type f -name '*.so*' \) | LC_ALL=C sort)

echo "== arm64 game image: packing reproducibly"
find "$game_root" -mindepth 1 -exec touch -hcd @0 {} +
(cd "$game_root" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet) \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs-game.cpio.gz"
if [ -n "${rom_sha:-}" ]; then
    printf '%s\n' "$rom_sha" >"$ARM64_ART_DIR/initramfs-game.rom.sha256"
fi
echo "ok: $ARM64_ART_DIR/initramfs-game.cpio.gz"
