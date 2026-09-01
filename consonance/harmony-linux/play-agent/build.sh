#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the play-agent for a guest image. The established SMB profile remains
# dynamic because it dlopens a libretro core. The Nova CI profile supplies a
# pinned QuickNES archive and produces a fully static binary: Harmony denies
# ring-3 RDTSC, which current glibc's dynamic loader executes before `main`.
# Run on x86-64 Linux; emit the binary path on stdout's last line.
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(uname -sm)" != "Linux x86_64" ]; then
    echo "play-agent: guest build needs x86-64 Linux (the box); use 'cargo test' for the portable gates" >&2
    exit 1
fi

if [ -n "${HARMONY_QUICKNES_STATIC_LIB:-}" ]; then
    static_flags=${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS:+$CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS }
    static_flags="${static_flags}-C target-feature=+crt-static"
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS=$static_flags \
        cargo build --locked --target x86_64-unknown-linux-gnu --release \
            --features static-quicknes --bin play-agent >&2
    agent_bin=$PWD/target/x86_64-unknown-linux-gnu/release/play-agent
else
    cargo build --locked --release --bin play-agent >&2
    agent_bin=$PWD/target/release/play-agent
fi
echo "$agent_bin"
