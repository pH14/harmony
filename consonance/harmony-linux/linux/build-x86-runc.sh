#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh
require_linux_amd64
require_tools cc make patch python3 readelf tar

go_archive=$DL_DIR/$(basename "$GO_X86_BOOTSTRAP_URL")
[ "$(sha256_of "$go_archive")" = "$GO_X86_BOOTSTRAP_SHA256" ] || {
    echo "FAIL: pinned amd64 Go archive hash mismatch" >&2
    exit 1
}
go_toolchain=$BUILD_ROOT/go-$GO_BOOTSTRAP_VERSION-amd64
rm -rf "$go_toolchain"
mkdir -p "$go_toolchain"
tar -xf "$go_archive" -C "$go_toolchain" --strip-components=1
extract_runc_source
extract_musl
extract_kernel
kernel_headers=$BUILD_ROOT/kernel-headers-x86-runc
mkdir -p "$kernel_headers"
make -C "$KSRC" ARCH=x86 INSTALL_HDR_PATH="$kernel_headers" headers_install >/dev/null
musl_source=$BUILD_ROOT/musl-x86-runc
musl_prefix=$BUILD_ROOT/musl-x86-runc-prefix
rm -rf "$musl_source" "$musl_prefix"
cp -a "$MUSLSRC" "$musl_source"
(
    cd "$musl_source"
    CC=cc CFLAGS='-O2 -march=x86-64' ./configure --prefix="$musl_prefix" --disable-shared >/dev/null
    make -j4 >/dev/null
    make install >/dev/null
)
export GOCACHE="$BUILD_ROOT/go-build-cache"
export GOPATH="$BUILD_ROOT/go-workspace"
export GOROOT=$go_toolchain
export PATH=$GOROOT/bin:$PATH
export GOOS=linux GOARCH=amd64 GOAMD64=v1 CGO_ENABLED=1
export CC=$musl_prefix/bin/musl-gcc
export CGO_CFLAGS="-O2 -march=x86-64 -isystem $kernel_headers/include"
export GOENV=off GOTOOLCHAIN=local GOPROXY=off GOSUMDB=off GOWORK=off GOFLAGS=
runc_output=$BUILD_ROOT/runc-x86
(
    cd "$BUILD_ROOT/runc-$RUNC_VERSION"
    go test -p=4 -mod=vendor -trimpath -buildvcs=false \
        -tags 'netgo osusergo urfave_cli_no_docs' \
        -ldflags '-linkmode external -extldflags -static -buildid=' \
        ./libcontainer -run '^TestHarmonyReexecEagerBinding$' -count=1
    go build -p=4 -mod=vendor -trimpath -buildvcs=false \
        -tags 'netgo osusergo urfave_cli_no_docs' \
        -ldflags '-linkmode external -extldflags -static -buildid=' \
        -o "$runc_output"
)
verify_static_runc "$runc_output" x86_64
mkdir -p "$X86_64_ART_DIR"
install -m 0755 "$runc_output" "$X86_64_ART_DIR/runc"
sha256_of "$X86_64_ART_DIR/runc" >"$X86_64_ART_DIR/runc.sha256"
{
    printf 'runc_source_sha256=%s\ngo_bootstrap_sha256=%s\nmusl_source_sha256=%s\n' \
        "$RUNC_SOURCE_SHA256" "$GO_X86_BOOTSTRAP_SHA256" "$MUSL_SHA256"
    printf 'kernel_source_sha256=%s\n' "$KERNEL_SHA256"
    printf 'go_version=%s\nc_compiler=%s\n' "$(go version)" "$(cc --version | head -1)"
    for runc_patch in "$LINUX_DIR"/patches/runc/*.patch; do
        printf 'patch_%s=%s\n' "$(basename "$runc_patch")" "$(sha256_of "$runc_patch")"
    done
    printf 'runc_sha256=%s\n' "$(sha256_of "$X86_64_ART_DIR/runc")"
} >"$X86_64_ART_DIR/runc-build.manifest"
echo "ok: $X86_64_ART_DIR/runc"
