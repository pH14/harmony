#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the M2 guest agent natively on the pinned Linux/aarch64 host, then
# fail closed if its complete linked image contains a live counter or LL/SC.
set -euo pipefail

cd "$(dirname "$0")"

if [ "$(uname -sm)" != "Linux aarch64" ]; then
    echo "FAIL: tetanes-agent guest build needs native Linux/aarch64 (validated on msr1)" >&2
    exit 1
fi

repo_root=$(cd ../../.. && pwd)
target=aarch64-unknown-linux-musl
musl_prefix=${HARMONY_MUSL_PREFIX:-}
[ -x "$musl_prefix/bin/musl-gcc" ] || {
    echo "FAIL: HARMONY_MUSL_PREFIX must name the owned static musl prefix" >&2
    exit 1
}
[ -f "$musl_prefix/lib/libc.a" ] || {
    echo "FAIL: HARMONY_MUSL_PREFIX has no static libc.a" >&2
    exit 1
}

rustc_command=$(command -v rustc)
sysroot=$($rustc_command --print sysroot)
if [ -n "${HARMONY_RUST_SOURCE_ROOT:-}" ]; then
    [ -f "$HARMONY_RUST_SOURCE_ROOT/Cargo.lock" ] || {
        echo "FAIL: HARMONY_RUST_SOURCE_ROOT has no Cargo.lock" >&2
        exit 1
    }
    [ -f "${HARMONY_RUST_LIBUNWIND:-}" ] || {
        echo "FAIL: HARMONY_RUST_LIBUNWIND does not name libunwind.a" >&2
        exit 1
    }
    real_sysroot=$sysroot
    sysroot=${GUEST_BUILD_ROOT:-$PWD/target}/rust-sysroot
    rustc_wrapper=${GUEST_BUILD_ROOT:-$PWD/target}/rust-bin/rustc
    rm -rf "$sysroot" "$(dirname "$rustc_wrapper")"
    mkdir -p "$sysroot/lib/rustlib" "$(dirname "$rustc_wrapper")"
    for entry in "$real_sysroot/lib/rustlib"/*; do
        ln -s "$entry" "$sysroot/lib/rustlib/$(basename "$entry")"
    done
    mkdir -p "$sysroot/lib/rustlib/src/rust"
    ln -s "$HARMONY_RUST_SOURCE_ROOT" \
        "$sysroot/lib/rustlib/src/rust/library"
    mkdir -p "$sysroot/lib/rustlib/$target/lib/self-contained"
    ln -s "$HARMONY_RUST_LIBUNWIND" \
        "$sysroot/lib/rustlib/$target/lib/self-contained/libunwind.a"
    cat >"$rustc_wrapper" <<EOF
#!/bin/sh
exec "$rustc_command" --sysroot "$sysroot" "\$@"
EOF
    chmod +x "$rustc_wrapper"
    export RUSTC=$rustc_wrapper
fi
rust_src=$sysroot/lib/rustlib/src/rust/library/Cargo.lock
[ -f "$rust_src" ] || {
    echo "FAIL: rust-src is required to rebuild the standard library LSE-only" >&2
    echo "install it with: rustup component add rust-src" >&2
    exit 1
}
rust_unwind=$sysroot/lib/rustlib/$target/lib/self-contained/libunwind.a
[ -f "$rust_unwind" ] || {
    echo "FAIL: Rust's $target support is required for the static unwind runtime" >&2
    echo "install it with: rustup target add $target" >&2
    exit 1
}

# The distributed aarch64 standard library contains baseline outline-atomic
# helpers with dormant LL/SC fallbacks. Rebuild std with the same architecture
# contract as the agent and link it against the owned static musl runtime so the
# complete shipped image is LSE-only and has no distro-library closure.
gcc_crt=$(dirname "$(cc -print-file-name=crtbegin.o)")
export RUSTC_BOOTSTRAP=1
export RUSTFLAGS="-C target-cpu=generic -C target-feature=+lse,-outline-atomics -C panic=abort \
-C link-arg=-Wl,--build-id=none -L native=$musl_prefix/lib -L native=$gcc_crt"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=$musl_prefix/bin/musl-gcc
cargo build --locked --release --bin harmony-tetanes-agent \
    --target "$target" -Z build-std=std,panic_abort
agent=$PWD/target/$target/release/harmony-tetanes-agent

if readelf -l "$agent" | grep -q 'INTERP'; then
    echo "FAIL: TetaNES agent has a dynamic interpreter" >&2
    exit 1
fi
if readelf -d "$agent" 2>/dev/null | grep -q '(NEEDED)'; then
    echo "FAIL: TetaNES agent has a dynamic library dependency" >&2
    exit 1
fi

python3 "$repo_root/consonance/harmony-linux/scripts/aa4-exclusive-scan.py" "$agent"
python3 "$repo_root/consonance/harmony-linux/scripts/aa5-counter-scan.py" "$agent"
echo "$agent"
