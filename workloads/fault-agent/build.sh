#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the fault agent for the faultlab guest image: a fully static musl
# binary, because Harmony denies ring-3 RDTSC and glibc's dynamic loader
# executes one before `main`. Run on native Linux (x86-64 or arm64); emit the
# binary path on stdout's last line.
set -euo pipefail
cd "$(dirname "$0")"

os=$(uname -s)
arch=$(uname -m)
if [ "$os" != Linux ]; then
    echo "fault-agent: guest build needs native Linux (x86_64 or aarch64); use 'cargo test' for the portable gates" >&2
    exit 1
fi

case "$arch" in
    x86_64)
        target=x86_64-unknown-linux-musl
        rustflags_var=CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS
        ;;
    aarch64|arm64)
        target=aarch64-unknown-linux-musl
        rustflags_var=CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS
        ;;
    *)
        echo "fault-agent: unsupported Linux architecture '$arch'; expected x86_64 or aarch64" >&2
        exit 1
        ;;
esac

rustup target list --installed | grep -qx "$target" || {
    echo "FAIL: install the guest target with: rustup target add $target" >&2
    exit 1
}

flags="-C target-feature=+crt-static"
if [ -n "${HARMONY_BUILD_PATH_PREFIX:-}" ]; then
    flags="$flags --remap-path-prefix=$HARMONY_BUILD_PATH_PREFIX=/build"
fi
env "$rustflags_var=$flags" \
    cargo build --locked --release --target "$target" --bin fault-agent >&2
agent=$PWD/target/$target/release/fault-agent

if readelf -l "$agent" | grep -q 'INTERP'; then
    echo "FAIL: fault agent has a dynamic interpreter" >&2
    exit 1
fi
if readelf -d "$agent" 2>/dev/null | grep -q '(NEEDED)'; then
    echo "FAIL: fault agent has a dynamic library dependency" >&2
    exit 1
fi

echo "$agent"
