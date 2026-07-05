#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the flow agent as a static x86-64 Linux binary for baking into a workload
# initramfs (task 61). Emits the binary path on stdout (last line) so the image
# builder can `install` it:
#
#   FLOW_AGENT_BIN="$(guest/flow-agent/build-static.sh)" \
#     sudo guest/linux/build-k3s-image.sh
#
# Uses the musl target for a fully static binary with no libc runtime dependency
# (the initramfs has no dynamic loader). Install the target once with:
#   rustup target add x86_64-unknown-linux-musl
set -eu

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
TARGET=x86_64-unknown-linux-musl

# RUSTFLAGS pins a fully-static CRT so the ELF needs no /lib/ld-musl loader.
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
  cargo build --release --manifest-path "$HERE/Cargo.toml" \
    --target "$TARGET" --bin flow-agent 1>&2

echo "$HERE/target/$TARGET/release/flow-agent"
