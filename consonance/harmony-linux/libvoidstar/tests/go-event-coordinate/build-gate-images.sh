#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the positive and negative gate-2 guest images on supported Linux/amd64.
set -euo pipefail

usage() {
    echo "usage: build-gate-images.sh BASE_INITRAMFS OUTPUT_DIR" >&2
    exit 2
}

[ "$#" -eq 2 ] || usage
base_initramfs=$1
output=$2
[ -f "$base_initramfs" ] || { echo "missing base initramfs: $base_initramfs" >&2; exit 1; }
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || {
    echo "gate 2 requires Linux/amd64: Antithesis SDK Go v0.8.0's native bridge loader is linux/amd64-only" >&2
    exit 1
}
case "$(go version)" in
    "go version go1.24."*" linux/amd64") ;;
    *) echo "gate 2 requires Go 1.24.x for the pinned instrumentor" >&2; exit 1 ;;
esac

root=$(cd "$(dirname "$0")" && pwd)
bridge=$(cd "$root/../.." && pwd)
mkdir -p "$output"
output=$(cd "$output" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/harmony-go-event-coordinate.XXXXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT HUP INT TERM

export GOFLAGS=-trimpath
export GOTOOLCHAIN=local
export CGO_ENABLED=1
export GOOS=linux
export GOARCH=amd64
export ANTITHESIS_SYMBOLS_DIR="$output/symbols"
export ANTITHESIS_SYMBOL_PREFIX=go-event-coordinate

go install github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v0.8.0
instrumentor=$(go env GOPATH)/bin/antithesis-go-toolexec
[ -x "$instrumentor" ] || { echo "pinned instrumentor was not installed" >&2; exit 1; }
sdk_dir=$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v0.8.0
[ -d "$sdk_dir" ] || { echo "pinned SDK module was not downloaded" >&2; exit 1; }
export ANTITHESIS_SDK_MODULE_DIR=$sdk_dir

rm -rf "$ANTITHESIS_SYMBOLS_DIR"
mkdir -p "$ANTITHESIS_SYMBOLS_DIR"
(cd "$root" && go build -a \
    -toolexec="$instrumentor" -buildvcs=false \
    -o "$output/go-event-coordinate.instrumented" .)
(cd "$root" && go build -buildvcs=false -o "$output/go-event-coordinate.stock" .)

set -- "$ANTITHESIS_SYMBOLS_DIR"/*.sym.tsv
[ "$#" -eq 1 ] && [ -s "$1" ] || {
    echo "instrumented fixture must produce exactly one nonempty symbol table" >&2
    exit 1
}
grep -a -F -q /usr/lib/libvoidstar.so "$output/go-event-coordinate.instrumented" || {
    echo "instrumented fixture did not link the Antithesis native loader" >&2
    exit 1
}
if grep -a -F -q /usr/lib/libvoidstar.so "$output/go-event-coordinate.stock"; then
    echo "stock negative-control fixture unexpectedly linked the Antithesis native loader" >&2
    exit 1
fi

make -C "$bridge" BUILD_DIR="$output/libvoidstar" clean all

copy_runtime_closure() {
    local executable=$1 destination=$2 dependency
    while IFS= read -r dependency; do
        [ -f "$dependency" ] || { echo "missing runtime dependency: $dependency" >&2; exit 1; }
        mkdir -p "$destination$(dirname "$dependency")"
        cp -L "$dependency" "$destination$dependency"
    done < <(
        ldd "$executable" | awk '
            /=> \/[^ ]+/ { print $3 }
            /^[[:space:]]*\/[^ ]*ld-linux[^ ]*/ { print $1 }
        ' | sort -u
    )
}

assemble() {
    local kind=$1 executable=$2 tree="$work/$1"
    mkdir -p "$tree"
    (cd "$tree" && gzip -dc "$base_initramfs" | cpio -idmu 2>/dev/null)
    install -m 0755 "$executable" "$tree/go-event-coordinate"
    install -m 0755 "$output/libvoidstar/libvoidstar.so" "$tree/usr/lib/libvoidstar.so"
    copy_runtime_closure "$executable" "$tree"
    copy_runtime_closure "$output/libvoidstar/libvoidstar.so" "$tree"
    (cd "$tree" && find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip -9 \
        >"$output/go-event-coordinate.$kind.cpio.gz")
}

assemble instrumented "$output/go-event-coordinate.instrumented"
assemble stock "$output/go-event-coordinate.stock"
sha256sum \
    "$output/go-event-coordinate.instrumented" \
    "$output/go-event-coordinate.stock" \
    "$output/go-event-coordinate.instrumented.cpio.gz" \
    "$output/go-event-coordinate.stock.cpio.gz" \
    "$ANTITHESIS_SYMBOLS_DIR"/*.sym.tsv >"$output/MANIFEST.sha256"
cat "$output/MANIFEST.sha256"
