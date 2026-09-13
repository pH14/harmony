#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the generic NES OCI image.
#
# The image carries only a small static BusyBox and the pinned static QuickNES
# play-agent. The host preparation API supplies each ROM as a read-only
# external input and the platform runtime supplies PID 1 and supervision.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
LINUX_DIR=$REPO_ROOT/consonance/harmony-linux/linux

# The shared helper owns the pinned BusyBox/musl sources, fixed build paths,
# SOURCE_DATE_EPOCH, and native-host checks used by the established images.
cd "$LINUX_DIR"
# shellcheck source=../../consonance/harmony-linux/linux/lib-build.sh disable=SC1091
. ./lib-build.sh

require_tools cargo cut make nproc patch readelf rustc sha256sum tail tar wc

quicknes_archive=${HARMONY_QUICKNES_STATIC_LIB:-}
agent_override=${PLAY_AGENT_BIN:-}
if [ -z "$agent_override" ]; then
    [ -n "$quicknes_archive" ] || {
        echo "FAIL: set HARMONY_QUICKNES_STATIC_LIB to libquicknes_libretro.a" >&2
        exit 1
    }
    [ -f "$quicknes_archive" ] || {
        echo "FAIL: QuickNES archive missing: $quicknes_archive" >&2
        exit 1
    }
    [ "$(basename "$quicknes_archive")" = libquicknes_libretro.a ] || {
        echo "FAIL: QuickNES archive must be named libquicknes_libretro.a" >&2
        exit 1
    }
fi

NES_ROOT=$BUILD_ROOT/nes-base-root
NES_BUSYBOX_OBJ=$BUILD_ROOT/busybox-build-nes
mkdir -p "$ART_DIR"

enable_busybox_symbol() {
    local symbol=$1
    if grep -qxF "CONFIG_${symbol}=y" "$NES_BUSYBOX_OBJ/.config"; then
        return
    fi
    grep -qxF "# CONFIG_${symbol} is not set" "$NES_BUSYBOX_OBJ/.config" || {
        echo "FAIL: BusyBox has no disabled CONFIG_${symbol} setting" >&2
        exit 1
    }
    sed "s/^# CONFIG_${symbol} is not set$/CONFIG_${symbol}=y/" \
        "$NES_BUSYBOX_OBJ/.config" >"$NES_BUSYBOX_OBJ/.config.tmp"
    mv "$NES_BUSYBOX_OBJ/.config.tmp" "$NES_BUSYBOX_OBJ/.config"
}

build_x86_busybox() {
    require_linux_amd64
    extract_busybox
    prepare_busybox_build_source
    rm -rf "$NES_BUSYBOX_OBJ"
    mkdir -p "$NES_BUSYBOX_OBJ"
    make -C "$BBSRC" O="$NES_BUSYBOX_OBJ" allnoconfig >/dev/null
    for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKNOD CHMOD CAT ECHO \
        GREP HALT REBOOT; do
        enable_busybox_symbol "$symbol"
    done
    grep -qxF 'CONFIG_EXTRA_CFLAGS=""' "$NES_BUSYBOX_OBJ/.config" || {
        echo "FAIL: BusyBox default compiler flags changed" >&2
        exit 1
    }
    set +o pipefail
    yes '' | make -C "$BBSRC" O="$NES_BUSYBOX_OBJ" oldconfig >/dev/null
    set -o pipefail
    make -C "$BBSRC" O="$NES_BUSYBOX_OBJ" -j"$(nproc)" busybox >/dev/null
}

build_arm64_busybox() {
    require_linux_aarch64
    extract_busybox
    prepare_busybox_build_source
    build_arm64_musl
    rm -rf "$NES_BUSYBOX_OBJ"
    mkdir -p "$NES_BUSYBOX_OBJ"
    make -C "$BBSRC" O="$NES_BUSYBOX_OBJ" allnoconfig >/dev/null
    for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKNOD CHMOD CAT ECHO \
        GREP HALT REBOOT; do
        enable_busybox_symbol "$symbol"
    done
    grep -qxF 'CONFIG_EXTRA_CFLAGS=""' "$NES_BUSYBOX_OBJ/.config" || {
        echo "FAIL: BusyBox default compiler flags changed" >&2
        exit 1
    }
    sed 's/^CONFIG_EXTRA_CFLAGS=""$/CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"/' \
        "$NES_BUSYBOX_OBJ/.config" >"$NES_BUSYBOX_OBJ/.config.tmp"
    mv "$NES_BUSYBOX_OBJ/.config.tmp" "$NES_BUSYBOX_OBJ/.config"
    set +o pipefail
    yes '' | make -C "$BBSRC" O="$NES_BUSYBOX_OBJ" oldconfig >/dev/null
    set -o pipefail
    make -C "$BBSRC" O="$NES_BUSYBOX_OBJ" CC="$ARM64_MUSL_PREFIX/bin/musl-gcc" \
        -j"$(nproc)" busybox >/dev/null
}

build_agent_x86() {
    if [ -n "$agent_override" ]; then
        printf '%s\n' "$agent_override"
        return
    fi
    local output
    output=$(HARMONY_QUICKNES_STATIC_LIB="$quicknes_archive" \
        bash "$SCRIPT_DIR/build.sh" | tail -1)
    printf '%s\n' "$output"
}

build_agent_arm64() {
    if [ -n "$agent_override" ]; then
        printf '%s\n' "$agent_override"
        return
    fi
    local play_target=aarch64-unknown-linux-musl
    local agent_target=$BUILD_ROOT/play-agent-target
    local rust_sysroot rust_unwind_dir agent_rustflags
    rust_sysroot=$(rustc --print sysroot)
    [ -d "$rust_sysroot/lib/rustlib/src/rust/library" ] || {
        echo "FAIL: rust-src is required for the ARM64 static play-agent" >&2
        exit 1
    }
    rust_unwind_dir=$rust_sysroot/lib/rustlib/$play_target/lib/self-contained
    [ -f "$rust_unwind_dir/libunwind.a" ] || {
        echo "FAIL: rust target $play_target with static libunwind is required" >&2
        exit 1
    }
    agent_rustflags=${RUSTFLAGS:-}
    agent_rustflags="${agent_rustflags:+$agent_rustflags }-C target-feature=+lse,-outline-atomics -C panic=abort -C link-self-contained=no -C link-arg=-Wl,--build-id=none -C link-arg=-L$ARM64_MUSL_PREFIX/lib -C link-arg=-L$rust_unwind_dir"
    if [ -n "${HARMONY_BUILD_PATH_PREFIX:-}" ]; then
        agent_rustflags="$agent_rustflags --remap-path-prefix=$HARMONY_BUILD_PATH_PREFIX=/build"
    fi
    rm -rf "$agent_target"
    (
        cd "$REPO_ROOT/workloads/nes-guest"
        RUSTC_BOOTSTRAP=1 \
            RUSTFLAGS="$agent_rustflags" \
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$ARM64_MUSL_PREFIX/bin/musl-gcc" \
            CARGO_TARGET_DIR="$agent_target" \
            HARMONY_QUICKNES_STATIC_LIB="$quicknes_archive" \
            cargo build --locked --release --target "$play_target" \
                --features static-quicknes --bin play-agent \
                -Z build-std=std,panic_abort
    )
    printf '%s\n' "$agent_target/$play_target/release/play-agent"
}

case "$(uname -m)" in
    x86_64)
        OCI_ARCH=amd64
        build_x86_busybox
        agent=$(build_agent_x86)
        ;;
    aarch64)
        OCI_ARCH=arm64
        build_arm64_busybox
        agent=$(build_agent_arm64)
        ;;
    *)
        echo "FAIL: generic NES OCI image must build on Linux/x86_64 or Linux/aarch64" >&2
        exit 1
        ;;
esac

[ -x "$agent" ] || {
    echo "FAIL: static play-agent missing: $agent" >&2
    exit 1
}
if readelf -l "$agent" | grep -q '[[:space:]]INTERP[[:space:]]'; then
    echo "FAIL: play-agent has a dynamic loader: $agent" >&2
    exit 1
fi
if readelf -d "$agent" 2>/dev/null | grep -q '(NEEDED)'; then
    echo "FAIL: play-agent has a dynamic library dependency: $agent" >&2
    exit 1
fi

rm -rf "$NES_ROOT"
mkdir -p "$NES_ROOT"/{bin,etc,tmp,opt/harmony}
install -m 0755 "$NES_BUSYBOX_OBJ/busybox" "$NES_ROOT/bin/busybox"
for applet in sh mount mknod chmod cat echo grep halt reboot; do
    ln -sf busybox "$NES_ROOT/bin/$applet"
done
install -m 0755 "$agent" "$NES_ROOT/opt/harmony/play-agent"
printf 'root:x:0:0:root:/root:/bin/sh\n' >"$NES_ROOT/etc/passwd"
printf 'root:x:0:\n' >"$NES_ROOT/etc/group"

# Build one deterministic OCI layer. The image deliberately contains no /init,
# /game.nes, supervisor, or execution control files. Those belong to the
# platform runtime and host-side prepared execution respectively.
find "$NES_ROOT" -mindepth 1 -exec touch -hcd @0 {} +
layer="$BUILD_ROOT/nes-oci-layer.tar"
tar --format=ustar --sort=name --mtime='@0' --owner=0 --group=0 \
    --numeric-owner -cf "$layer" -C "$NES_ROOT" .
layer_digest=$(sha256_of "$layer")
layer_size=$(wc -c <"$layer")
layer_size=$((layer_size))

config="$BUILD_ROOT/nes-oci-config.json"
cat >"$config" <<EOF
{"architecture":"$OCI_ARCH","os":"linux","config":{"Env":["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],"Entrypoint":[],"Cmd":[],"WorkingDir":"/","User":"0:0"},"rootfs":{"type":"layers","diff_ids":["sha256:$layer_digest"]},"history":[{"created":"1970-01-01T00:00:00Z","created_by":"harmony nes workload image"}]}
EOF
config_digest=$(sha256_of "$config")
config_size=$(wc -c <"$config")
config_size=$((config_size))

manifest="$BUILD_ROOT/nes-oci-manifest.json"
cat >"$manifest" <<EOF
{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:$config_digest","size":$config_size},"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"sha256:$layer_digest","size":$layer_size}]}
EOF
manifest_digest=$(sha256_of "$manifest")
manifest_size=$(wc -c <"$manifest")
manifest_size=$((manifest_size))

image="$ART_DIR/nes.oci"
staging="$ART_DIR/.nes.oci.tmp"
rm -rf "$staging" "$image"
mkdir -p "$staging/blobs/sha256"
install -m 0644 "$layer" "$staging/blobs/sha256/$layer_digest"
install -m 0644 "$config" "$staging/blobs/sha256/$config_digest"
install -m 0644 "$manifest" "$staging/blobs/sha256/$manifest_digest"
cat >"$staging/index.json" <<EOF
{"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:$manifest_digest","size":$manifest_size,"platform":{"architecture":"$OCI_ARCH","os":"linux"}}]}
EOF
printf '%s\n' '{"imageLayoutVersion":"1.0.0"}' >"$staging/oci-layout"
mv "$staging" "$image"

printf '%s  nes.oci/blobs/sha256/%s\n' "$manifest_digest" "$manifest_digest" \
    >"$ART_DIR/nes.oci.sha256"
echo "ok: $image (OCI layout; architecture=$OCI_ARCH)"
