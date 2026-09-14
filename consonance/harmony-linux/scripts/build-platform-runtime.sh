#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

cd "$(dirname "$0")/../linux"
# shellcheck source=../linux/lib-build.sh disable=SC1091
. ./lib-build.sh

runtime_toolchain=nightly-2026-06-16
runtime_arch=$(uname -m)
repo=$(cd "$GUEST_DIR/../.." && pwd)
require_tools cargo rustc rustup python3
case "$runtime_arch" in
    x86_64)
        require_linux_amd64
        runtime_target=x86_64-unknown-linux-musl
        image_arch=amd64
        kernel_builder=./build-kernel.sh
        ;;
    aarch64)
        require_linux_aarch64
        runtime_target=aarch64-unknown-linux-musl
        image_arch=arm64
        kernel_builder=./build-arm64-kernel.sh
        ;;
    *) echo "FAIL: unsupported platform architecture: $runtime_arch" >&2; exit 1 ;;
esac

runtime_sysroot=$(rustc +"$runtime_toolchain" --print sysroot)
[ -d "$runtime_sysroot/lib/rustlib/$runtime_target/lib" ] || {
    echo "FAIL: install $runtime_target for $runtime_toolchain" >&2; exit 1;
}
flags=("--remap-path-prefix=$repo=/src" "--remap-path-prefix=$BUILD_ROOT=/build"
       "--remap-path-prefix=$runtime_sysroot=/rust" -C panic=abort -C "link-arg=-Wl,--build-id=none")
cargo_options=()
if [ "$runtime_arch" = aarch64 ]; then
    [ -d "$runtime_sysroot/lib/rustlib/src/rust/library" ] || {
        echo "FAIL: install rust-src for $runtime_toolchain" >&2; exit 1;
    }
    flags+=(-C "target-feature=+lse,-outline-atomics" -C link-self-contained=no
            -C "link-arg=-L$ARM64_MUSL_PREFIX/lib"
            -C "link-arg=-L$runtime_sysroot/lib/rustlib/$runtime_target/lib/self-contained")
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$ARM64_MUSL_PREFIX/bin/musl-gcc"
    cargo_options+=(-Z "build-std=std,panic_abort")
fi
printf -v encoded_flags '%s\x1f' "${flags[@]}"
export CARGO_ENCODED_RUSTFLAGS=${encoded_flags%$'\x1f'}
unset RUSTFLAGS

build_guest() {
    local component=$1 binary=$2
    local target_dir=$BUILD_ROOT/rust-$component
    CARGO_TARGET_DIR="$target_dir" cargo +"$runtime_toolchain" build --locked --release \
        --manifest-path "$GUEST_DIR/$component/Cargo.toml" --target "$runtime_target" \
        "${cargo_options[@]}"
    if [ "$runtime_arch" = aarch64 ]; then
        python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$target_dir/$runtime_target/release/$binary"
        python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$target_dir/$runtime_target/release/$binary"
    fi
}
"$kernel_builder"
if [ "$runtime_arch" = aarch64 ]; then
    build_arm64_musl
fi
build_guest supervisor harmony-supervisor
build_guest runtime-fixture runtime-fixture
export HARMONY_RUNTIME_INIT="$GUEST_DIR/runtime/init.sh"
export HARMONY_RUNTIME_SUPERVISOR="$BUILD_ROOT/rust-supervisor/$runtime_target/release/harmony-supervisor"
./build-oci-runtime-initramfs.sh "$runtime_arch"

fixture=$ART_DIR/$runtime_arch/fixture
if [ -e "$fixture" ]; then
    rm -rf "$fixture"
fi
python3 "$GUEST_DIR/runtime-fixture/package.py" \
    "$BUILD_ROOT/rust-runtime-fixture/$runtime_target/release/runtime-fixture" \
    "$fixture" --architecture "$image_arch"
python3 "$GUEST_DIR/scripts/runtime-artifacts.py" seal --repo "$repo" \
    --architecture "$runtime_arch" --artifacts "$ART_DIR/$runtime_arch"
