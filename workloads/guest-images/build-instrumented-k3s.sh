#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

workload_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$workload_dir/../.." && pwd)
DL_DIR=${HARMONY_DOWNLOAD_DIR:-$repo_root/consonance/harmony-linux/dl}
BUILD_ROOT=${GUEST_BUILD_ROOT:-/tmp/harmony-linux-build}

# shellcheck source=../../consonance/harmony-linux/scripts/lib.sh disable=SC1091
. "$repo_root/consonance/harmony-linux/scripts/lib.sh"
# shellcheck source=versions.lock disable=SC1091
. "$workload_dir/versions.lock"

source_tar=$DL_DIR/$(basename "$K3S_SOURCE_URL")
source_dir=$BUILD_ROOT/k3s-$K3S_SOURCE_COMMIT
output=$BUILD_ROOT/k3s-instrumented

[ -f "$source_tar" ] || { echo "FAIL: missing $source_tar; run the workload fetch first" >&2; exit 1; }
[ "$(sha256_of "$source_tar")" = "$K3S_SOURCE_SHA256" ] || {
    echo "FAIL: K3s source hash mismatch" >&2
    exit 1
}
rm -rf "$source_dir" "$output"
mkdir -p "$source_dir" "$output"
tar -xf "$source_tar" -C "$source_dir" --strip-components=1
mkdir -p "$source_dir/.harmony"
cp -a "$repo_root/consonance/harmony-linux/libvoidstar" "$source_dir/.harmony/"

docker build \
    --build-arg "GOLANG=$K3S_GO_IMAGE" \
    --build-arg "GIT_TAG=$K3S_VERSION" \
    --build-arg "COMMIT=$K3S_SOURCE_COMMIT" \
    --build-arg "BUILD_DATE=$K3S_BUILD_DATE" \
    --build-arg "ANTITHESIS_SDK_VERSION=$ANTITHESIS_GO_SDK_VERSION" \
    --file "$workload_dir/k3s-instrumented.Dockerfile" \
    --output "type=local,dest=$output" \
    "$source_dir"

[ -x "$output/k3s" ] || { echo "FAIL: K3s source build did not produce a launcher" >&2; exit 1; }
[ -s "$output/instrumented-server.sha256" ] || {
    echo "FAIL: K3s source build did not attest the embedded instrumented server" >&2
    exit 1
}
sha256sum "$output/k3s" >"$output/k3s.sha256"
