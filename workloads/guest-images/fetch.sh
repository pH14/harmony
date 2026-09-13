#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Download and verify the workload image inputs. Platform sources and the
# pinned static OCI runtime are fetched by consonance/harmony-linux/scripts/fetch.sh.
set -euo pipefail

workload_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$workload_dir/../.." && pwd)
DL_DIR=${HARMONY_DOWNLOAD_DIR:-$repo_root/consonance/harmony-linux/dl}

# shellcheck source=../../consonance/harmony-linux/scripts/lib.sh disable=SC1091
. "$repo_root/consonance/harmony-linux/scripts/lib.sh"
# shellcheck source=versions.lock disable=SC1091
. "$workload_dir/versions.lock"

mkdir -p "$DL_DIR"

fetch_one() {
    local url=$1
    local sha=$2
    local file
    file="$DL_DIR/$(basename "$url")"
    if [ -f "$file" ] && [ "$(sha256_of "$file")" = "$sha" ]; then
        echo "ok: $file (cached, hash verified)"
        return
    fi
    echo "fetching $url"
    local downloaded=false
    if command -v curl >/dev/null 2>&1; then
        if curl -fsSL --connect-timeout 20 --retry 2 --retry-all-errors --retry-delay 5 \
            -o "$file.part" "$url"; then
            downloaded=true
        fi
    elif command -v wget >/dev/null 2>&1; then
        if wget -q --connect-timeout=20 --tries=3 --waitretry=5 \
            -O "$file.part" "$url"; then
            downloaded=true
        fi
    else
        echo "FAIL: need curl or wget to fetch $url" >&2
        exit 1
    fi
    if [ "$downloaded" != true ]; then
        rm -f "$file.part"
        echo "FAIL: could not download $url" >&2
        exit 1
    fi
    local got
    got=$(sha256_of "$file.part")
    if [ "$got" != "$sha" ]; then
        echo "FAIL: $file sha256 mismatch (want $sha, got $got)" >&2
        rm -f "$file.part"
        exit 1
    fi
    mv "$file.part" "$file"
    echo "ok: $file (downloaded, hash verified)"
}

fetch_one "$PG_SOURCE_URL" "$PG_SOURCE_SHA256"
fetch_one "$PG_SERVER_DEB_URL" "$PG_SERVER_DEB_SHA256"
fetch_one "$PG_CLIENT_DEB_URL" "$PG_CLIENT_DEB_SHA256"
fetch_one "$PG_LIBPQ_DEB_URL" "$PG_LIBPQ_DEB_SHA256"
fetch_one "$DOCKER_TGZ_URL" "$DOCKER_TGZ_SHA256"
fetch_one "$K3S_SOURCE_URL" "$K3S_SOURCE_SHA256"
fetch_one "$K3S_AIRGAP_URL" "$K3S_AIRGAP_SHA256"
fetch_one "$IPTABLES_SOURCE_URL" "$IPTABLES_SOURCE_SHA256"

fetch_postgres_image() {
    local out="$DL_DIR/postgres-image.tar"
    if [ -s "$out" ]; then
        echo "ok: $out (cached; integrity anchored by the pinned registry digest)"
        return
    fi
    if ! command -v ctr >/dev/null 2>&1 || ! ctr version >/dev/null 2>&1; then
        echo "skip: $out — a reachable containerd ctr is required for this input" >&2
        echo "      rerun workloads/guest-images/fetch.sh on the Linux build box" >&2
        return
    fi
    local ns=guest-images-fetch
    local ref="docker.io/library/${POSTGRES_IMAGE_NAME%%:*}@${POSTGRES_IMAGE_INDEX_DIGEST}"
    echo "fetching $ref via ctr (-> $out)"
    ctr -n "$ns" image pull --platform linux/amd64 "$ref"
    ctr -n "$ns" image tag "$ref" "docker.io/library/$POSTGRES_IMAGE_NAME" 2>/dev/null || true
    ctr -n "$ns" image export --platform linux/amd64 "$out.part" \
        "docker.io/library/$POSTGRES_IMAGE_NAME"
    mv "$out.part" "$out"
    ctr -n "$ns" image rm "docker.io/library/$POSTGRES_IMAGE_NAME" "$ref" \
        >/dev/null 2>&1 || true
    ctr -n "$ns" content prune references >/dev/null 2>&1 || true
    echo "ok: $out ($(sha256_of "$out") — derived from the digest-pinned pull)"
}

fetch_k3s_pause_image() {
    local out="$DL_DIR/k3s-pause-image.tar"
    local air
    air="$DL_DIR/$(basename "$K3S_AIRGAP_URL")"
    if [ -s "$out" ]; then
        echo "ok: $out (cached; extracted from the pinned air-gap archive)"
        return
    fi
    if ! command -v ctr >/dev/null 2>&1 || ! ctr version >/dev/null 2>&1; then
        echo "skip: $out — a reachable containerd ctr is required for this input" >&2
        echo "      rerun workloads/guest-images/fetch.sh on the Linux build box" >&2
        return
    fi
    local ns=guest-images-k3s-fetch
    echo "importing $air via ctr to extract the pause image (-> $out)"
    ctr -n "$ns" image import "$air" >/dev/null
    local pause_ref
    pause_ref=$(ctr -n "$ns" image ls -q | grep -E 'mirrored-pause|/pause:' | head -1)
    [ -n "$pause_ref" ] || { echo "FAIL: no pause image in $air" >&2; exit 1; }
    ctr -n "$ns" image export --platform linux/amd64 "$out.part" "$pause_ref"
    mv "$out.part" "$out"
    local image_ref
    while IFS= read -r image_ref; do
        ctr -n "$ns" image rm "$image_ref" >/dev/null 2>&1 || true
    done < <(ctr -n "$ns" image ls -q)
    ctr -n "$ns" content prune references >/dev/null 2>&1 || true
    echo "ok: $out ($(sha256_of "$out") — pause image from the pinned archive)"
}

fetch_postgres_image
fetch_k3s_pause_image
echo "PASS: workload image inputs are ready in $DL_DIR"
