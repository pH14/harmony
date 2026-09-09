#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Ordinary, uninstrumented Go as /init. The locked Nix builder supplies Go;
# no runtime environment overrides or application-specific kernel are needed.
set -euo pipefail
cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh
require_linux_amd64
require_tools go cc gzip
extract_kernel
mkdir -p "$BUILD_ROOT/go-cache" "$ART_DIR"

(
    cd "$LINUX_DIR/go-runtime-guest"
    CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOAMD64=v1 \
        GOENV=off GOFLAGS='' GOWORK=off GOTOOLCHAIN=local GOPROXY=off GOSUMDB=off \
        GOCACHE="$BUILD_ROOT/go-cache" \
        go build -trimpath -buildvcs=false -ldflags=-buildid= \
            -o "$BUILD_ROOT/go-runtime-guest" .
)
cc -O2 -o "$BUILD_ROOT/gen_init_cpio" "$KSRC/usr/gen_init_cpio.c"
spec=$BUILD_ROOT/initramfs-go-runtime.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
file /init $BUILD_ROOT/go-runtime-guest 0755 0 0
EOF
"$BUILD_ROOT/gen_init_cpio" -t 0 "$spec" | gzip -n -9 >"$ART_DIR/initramfs-go-runtime.cpio.gz"
echo "ok: $ART_DIR/initramfs-go-runtime.cpio.gz ($(go version))"
