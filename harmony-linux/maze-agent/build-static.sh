#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the maze agent as a static x86-64 Linux binary for baking into the maze
# workload initramfs (task 134). Emits the binary path on stdout (last line) so
# the image builder can `install` it:
#
#   MAZE_AGENT_BIN="$(harmony-linux/maze-agent/build-static.sh)" \
#     harmony-linux/linux/build-maze-image.sh
#
# Uses the musl target for a fully static binary with no libc runtime dependency
# (the initramfs has no dynamic loader; unlike the play-agent nothing here
# dlopens, so the flow-agent static pattern applies verbatim). Install the
# target once with:
#   rustup target add x86_64-unknown-linux-musl
set -eu

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
TARGET=x86_64-unknown-linux-musl

# RUSTFLAGS pins a fully-static CRT so the ELF needs no /lib/ld-musl loader.
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
  cargo build --release --manifest-path "$HERE/Cargo.toml" \
    --target "$TARGET" --bin maze-agent 1>&2

echo "$HERE/target/$TARGET/release/maze-agent"
