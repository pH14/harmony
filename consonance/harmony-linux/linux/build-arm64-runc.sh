#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned arm64 runc source with the pinned Go runtime closure.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make objdump patch python3 readelf rsync tar

runc_source_tarball=$DL_DIR/$(basename "$RUNC_SOURCE_URL")
go_bootstrap_tarball=$DL_DIR/$(basename "$GO_BOOTSTRAP_URL")
runc_source=$BUILD_ROOT/runc-$RUNC_VERSION
go_pristine=$BUILD_ROOT/go-$GO_BOOTSTRAP_VERSION
go_toolchain=$BUILD_ROOT/go-$GO_BOOTSTRAP_VERSION-harmony-lse
go_unpack=$BUILD_ROOT/go-$GO_BOOTSTRAP_VERSION-unpack
kernel_headers=$BUILD_ROOT/kernel-headers-arm64
runc_output=$BUILD_ROOT/runc-lse

verify_archive() {
    local archive=$1
    local expected=$2
    [ -f "$archive" ] || {
        echo "FAIL: $archive missing — run 'make -C consonance/harmony-linux fetch' first" >&2
        exit 1
    }
    local got
    got=$(sha256_of "$archive")
    [ "$got" = "$expected" ] || {
        echo "FAIL: $archive sha256 mismatch (want $expected, got $got)" >&2
        exit 1
    }
}

verify_archive "$go_bootstrap_tarball" "$GO_BOOTSTRAP_SHA256"

# The Go archive contains a top-level `go` directory. Keep that verified
# extraction pristine and patch only a fresh mutable copy for this build.
rm -rf "$go_pristine" "$go_toolchain" "$go_unpack"
mkdir -p "$go_unpack"
tar -xf "$go_bootstrap_tarball" -C "$go_unpack"
[ -d "$go_unpack/go" ] || {
    echo "FAIL: Go bootstrap archive has no top-level go directory" >&2
    exit 1
}
mv "$go_unpack/go" "$go_pristine"
rmdir "$go_unpack"
cp -a "$go_pristine" "$go_toolchain"
for go_patch in "$LINUX_DIR"/patches/go/*.patch; do
    patch -d "$go_toolchain" --batch --forward -p1 <"$go_patch" >/dev/null
done

# Re-extract the source from the verified archive so no prior source mutation
# can enter the build. verify_and_extract checks the archive before extraction.
rm -rf "$runc_source"
verify_and_extract "$runc_source_tarball" "$RUNC_SOURCE_SHA256" "$runc_source"

# The runc cgo surface includes Linux UAPI headers. Export headers from the
# same pinned kernel source used by the platform build and make their location
# explicit to cgo and the musl compiler.
rm -rf "$KSRC"
extract_kernel
rm -rf "$kernel_headers"
mkdir -p "$kernel_headers"
make -C "$KSRC" ARCH=arm64 INSTALL_HDR_PATH="$kernel_headers" headers_install >/dev/null
[ -d "$kernel_headers/include/linux" ] || {
    echo "FAIL: kernel headers were not exported to $kernel_headers" >&2
    exit 1
}
export KERNEL_HEADERS=$kernel_headers

build_arm64_musl
[ -x "$ARM64_MUSL_PREFIX/bin/musl-gcc" ] || {
    echo "FAIL: freshly built arm64 musl compiler is unavailable" >&2
    exit 1
}

export GOROOT=$go_toolchain
export PATH=$GOROOT/bin:$PATH
export GOOS=linux
export GOARCH=arm64
export GOARM64=v8.1
export CGO_ENABLED=1
export CC=$ARM64_MUSL_PREFIX/bin/musl-gcc
export CGO_CFLAGS="-O2 -march=armv8.1-a+lse -mno-outline-atomics -isystem $KERNEL_HEADERS/include"
export GOENV=off
export GOTOOLCHAIN=local
export GOPROXY=off
export GOSUMDB=off
export GOWORK=off
export GOFLAGS=

rm -f "$runc_output"
(
    cd "$runc_source"
    go build -p=4 -mod=vendor -trimpath -buildvcs=false \
        -tags 'netgo osusergo urfave_cli_no_docs' \
        -ldflags '-linkmode external -extldflags -static -buildid=' \
        -o "$runc_output"
)

verify_static_runc "$runc_output" aarch64
python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$runc_output"
python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$runc_output"

mkdir -p "$ARM64_ART_DIR"
install -m 0755 "$runc_output" "$ARM64_ART_DIR/runc"
sha256_of "$ARM64_ART_DIR/runc" >"$ARM64_ART_DIR/runc.sha256"
echo "ok: $ARM64_ART_DIR/runc"
