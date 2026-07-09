#!/usr/bin/env bash
# Build the play-agent for the guest image. Unlike flow-agent this is a
# **dynamic** (glibc) build: the agent dlopens the pinned libretro core at
# runtime, and dlopen from a fully-static musl binary is unsupported — the
# image builder copies the ldd closure into the rootfs instead (the
# build-postgres-image.sh pattern). Run on the box (x86-64 Linux); emits the
# binary path on stdout's last line so the image builder can `install` it.
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(uname -sm)" != "Linux x86_64" ]; then
    echo "play-agent: guest build needs x86-64 Linux (the box); use 'cargo test' for the portable gates" >&2
    exit 1
fi

cargo build --release --bin play-agent >&2
echo "$PWD/target/release/play-agent"
