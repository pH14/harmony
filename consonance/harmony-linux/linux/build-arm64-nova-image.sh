#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the Nova-in-Consonance initramfs natively on the validated Linux/aarch64
# host (msr1). BusyBox, musl, the static QuickNES play-agent, and the ROM are
# all kept in an architecture-specific build so the established x86 image is
# never overwritten.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make gzip cpio cargo rustc python3 readelf patch git ar

# The ROM is an external, pinned workload input. A caller may also supply an
# already target-correct QuickNES archive; otherwise this script builds the
# pinned core itself after the patched-musl compiler exists.
#
#   dissonance/scripts/build-nova-rom.sh dissonance/nova-build
: "${HARMONY_NOVA_ROM:?set HARMONY_NOVA_ROM to the pinned built nova.nes (from dissonance/scripts/build-nova-rom.sh)}"
[ -f "$HARMONY_NOVA_ROM" ] || {
    echo "FAIL: Nova ROM missing: $HARMONY_NOVA_ROM" >&2
    exit 1
}
nova_core_static=${HARMONY_NOVA_CORE_STATIC:-}
if [ -n "$nova_core_static" ]; then
    [ -f "$nova_core_static" ] || {
        echo "FAIL: QuickNES archive missing: $nova_core_static" >&2
        exit 1
    }
    [ "$(basename "$nova_core_static")" = libquicknes_libretro.a ] || {
        echo "FAIL: QuickNES archive must be named libquicknes_libretro.a" >&2
        exit 1
    }
fi

nova_root=$BUILD_ROOT/arm64-nova-root
busybox_obj=$BUILD_ROOT/busybox-build-arm64-nova

echo "== arm64 Nova image: building LSE-only static musl ($MUSL_VERSION)"
build_arm64_game_musl
musl_cc=$ARM64_GAME_MUSL_PREFIX/bin/musl-gcc

if [ -z "$nova_core_static" ] && [ -z "${PLAY_AGENT_BIN:-}" ]; then
    echo "== arm64 Nova image: building pinned static QuickNES with patched musl"
    repo_root=$(cd "$GUEST_DIR/../.." && pwd)
    quicknes_build=$BUILD_ROOT/quicknes-arm64-nova
    mkdir -p "$quicknes_build"
    nova_core_static=$quicknes_build/libquicknes_libretro.a
    HARMONY_QUICKNES_STATIC_OUTPUT="$nova_core_static" \
        HARMONY_QUICKNES_CC="$musl_cc" \
        HARMONY_QUICKNES_CXX="$musl_cc" \
        HARMONY_QUICKNES_AR=ar \
        HARMONY_QUICKNES_CXXFLAGS='-march=armv8.1-a+lse -mno-outline-atomics' \
        HARMONY_QUICKNES_BUILD_JOBS="$(nproc)" \
        "$repo_root/scripts/build-quicknes-core.sh" \
        "$quicknes_build/quicknes_libretro.so"
fi

echo "== arm64 Nova image: building LSE-only static busybox ($BUSYBOX_VERSION)"
extract_busybox
prepare_busybox_build_source
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

# Keep the shipped command surface identical to the arm64 game image. Nova's
# init uses only a subset, but the explicit surface makes the image auditable.
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
        echo "FAIL: arm64 Nova BusyBox lost CONFIG_${symbol}" >&2
        exit 1
    }
done
grep -qxF 'CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"' \
    "$busybox_obj/.config" || {
    echo "FAIL: arm64 Nova BusyBox lost its LSE-only compiler flags" >&2
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
    echo "FAIL: arm64 Nova BusyBox applet surface changed" >&2
    echo "expected:" >&2
    printf '%s\n' "$expected_applet_table" >&2
    echo "actual:" >&2
    printf '%s\n' "$actual_applet_table" >&2
    exit 1
fi
if [ "$("$busybox_obj"/busybox echo dispatcher-ok)" != dispatcher-ok ]; then
    echo "FAIL: arm64 Nova BusyBox dispatcher cannot invoke an applet" >&2
    exit 1
fi

echo "== arm64 Nova image: building static aarch64 QuickNES play-agent"
if [ -n "${PLAY_AGENT_BIN:-}" ]; then
    agent=$PLAY_AGENT_BIN
    [ -x "$agent" ] || {
        echo "FAIL: PLAY_AGENT_BIN is not executable: $agent" >&2
        exit 1
    }
    echo "== arm64 Nova image: using prebuilt play-agent"
else
    play_target=aarch64-unknown-linux-musl
    agent_target=$BUILD_ROOT/play-agent-target
    rust_sysroot=$(rustc --print sysroot)
    [ -d "$rust_sysroot/lib/rustlib/src/rust/library" ] || {
        echo "FAIL: rust-src is required for the LSE-only static play-agent" >&2
        echo "      install it with: rustup component add rust-src" >&2
        exit 1
    }
    rust_unwind_dir=$rust_sysroot/lib/rustlib/$play_target/lib/self-contained
    [ -f "$rust_unwind_dir/libunwind.a" ] || {
        echo "FAIL: the pinned Rust $play_target target is required for static libunwind" >&2
        echo "      install it with: rustup target add $play_target" >&2
        exit 1
    }
    agent_rustflags=${RUSTFLAGS:-}
    # The musl target otherwise injects Rust's self-contained CRT names and
    # `-lunwind`, bypassing this image's patched musl wrapper. External-link
    # mode lets musl-gcc supply its absolute CRT paths; the version-matched Rust
    # static unwind archive below fills the one target runtime the wrapper does
    # not provide. build-std still rebuilds Rust code with the LSE-only flags.
    agent_rustflags="${agent_rustflags:+$agent_rustflags }-C target-feature=+lse,-outline-atomics -C panic=abort -C link-self-contained=no -C link-arg=-Wl,--build-id=none -C link-arg=-L$ARM64_GAME_MUSL_PREFIX/lib -C link-arg=-L$rust_unwind_dir"
    if [ -n "${HARMONY_BUILD_PATH_PREFIX:-}" ]; then
        agent_rustflags="$agent_rustflags --remap-path-prefix=$HARMONY_BUILD_PATH_PREFIX=/build"
    fi
    rm -rf "$agent_target"
    (
        cd "$GUEST_DIR/play-agent"
        RUSTC_BOOTSTRAP=1 \
            RUSTFLAGS="$agent_rustflags" \
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$musl_cc" \
            CARGO_TARGET_DIR="$agent_target" \
            HARMONY_QUICKNES_STATIC_LIB="$nova_core_static" \
            cargo build --locked --release --target "$play_target" \
                --features static-quicknes --bin play-agent \
                -Z build-std=std,panic_abort
    )
    agent=$agent_target/$play_target/release/play-agent
fi
[ -x "$agent" ] || { echo "FAIL: aarch64 Nova play-agent missing: $agent" >&2; exit 1; }
if ! readelf -h "$agent" | grep -q 'Machine:.*AArch64'; then
    echo "FAIL: Nova play-agent is not an AArch64 executable: $agent" >&2
    exit 1
fi
if readelf -l "$agent" | grep -q '[[:space:]]INTERP[[:space:]]'; then
    echo "FAIL: Nova play-agent has a dynamic loader" >&2
    exit 1
fi
if readelf -d "$agent" 2>/dev/null | grep -q '(NEEDED)'; then
    echo "FAIL: Nova play-agent has a dynamic library dependency" >&2
    exit 1
fi

echo "== arm64 Nova image: assembling rootfs"
rm -rf "$nova_root"
mkdir -p "$nova_root"/{bin,etc,proc,sys,dev,tmp,opt/harmony}
install -m 0755 "$busybox_obj/busybox" "$nova_root/bin/busybox"
for applet in sh mount mknod chmod cat echo grep halt reboot; do
    ln -sf busybox "$nova_root/bin/$applet"
done
install -m 0755 "$agent" "$nova_root/opt/harmony/play-agent"
install -m 0644 "$HARMONY_NOVA_ROM" "$nova_root/opt/harmony/nova.nes"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$nova_root/etc/passwd"
printf 'root:x:0:\n' >"$nova_root/etc/group"
rom_sha=$(sha256_of "$nova_root/opt/harmony/nova.nes")
printf '%s\n' "$rom_sha" >"$nova_root/opt/harmony/nova.nes.sha256"
install -m 0755 "$LINUX_DIR/nova-game-init.sh" "$nova_root/init"

# The entire shipped userspace, including the static QuickNES agent and the
# libc used by it, must remain free of LL/SC and host counter instructions.
echo "== arm64 Nova image: scanning every executable mapping"
while read -r binary; do
    if readelf -h "$binary" >/dev/null 2>&1; then
        python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$binary"
        python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$binary"
    fi
done < <(find "$nova_root" \( -type f -perm -0100 -o -type f -name '*.so*' \) | LC_ALL=C sort)

echo "== arm64 Nova image: packing reproducibly (ROM $rom_sha)"
find "$nova_root" -mindepth 1 -exec touch -hcd @0 {} +
(cd "$nova_root" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --reproducible --quiet) \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs-nova.cpio.gz"
printf '%s\n' "$rom_sha" >"$ARM64_ART_DIR/initramfs-nova.rom.sha256"
echo "ok: $ARM64_ART_DIR/initramfs-nova.cpio.gz"
