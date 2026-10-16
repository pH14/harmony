#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

if (( $# < 1 || $# > 3 )); then
    echo 'usage: build-consonance-cli-pair.sh OUTPUT_DIR [MANIFEST_PATH [TARGET_DIR]]' >&2
    exit 2
fi

output=$(realpath -m -- "$1")
manifest=$(realpath -m -- "${2:-Cargo.toml}")
target=$(realpath -m -- "${3:-target}")
test -f "$manifest"
command -v musl-gcc >/dev/null

cd "$(dirname "$manifest")"
rustup target add x86_64-unknown-linux-musl
cargo build --locked --release --manifest-path "$manifest" --target-dir "$target" \
    -p harmony-cli --bin harmony
cargo build --locked --release --manifest-path "$manifest" --target-dir "$target" \
    -p harmony-cli --target x86_64-unknown-linux-musl \
    --no-default-features --features consonance-host --bin harmony-consonance-host

mkdir -p "$output"
for binary in harmony harmony-consonance-host; do
    source="$target/release/$binary"
    if [[ "$binary" == harmony-consonance-host ]]; then
        source="$target/x86_64-unknown-linux-musl/release/$binary"
    fi
    destination="$output/$binary"
    if [[ ! "$source" -ef "$destination" ]]; then
        cp -p -- "$source" "$destination"
    fi
    test -x "$destination"
done
