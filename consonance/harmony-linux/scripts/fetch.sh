#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Download and verify the platform sources and pinned platform tools.
# Workload package inputs are fetched by their package-owned entrypoints.
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck source=lib.sh disable=SC1091
. scripts/lib.sh
# shellcheck source=../linux/versions.lock disable=SC1091
. linux/versions.lock

mkdir -p dl

fetch_one() {
    local url=$1
    local sha=$2
    local file
    file="dl/$(basename "$url")"
    if [ -f "$file" ] && [ "$(sha256_of "$file")" = "$sha" ]; then
        echo "ok: $file (cached, hash verified)"
        return
    fi
    echo "fetching $url"
    local sources=("$url")
    if [ -n "${3:-}" ]; then
        sources+=("$3")
    fi
    local downloaded=false
    local source_url
    for source_url in "${sources[@]}"; do
        if command -v curl >/dev/null 2>&1; then
            if curl -fsSL --connect-timeout 20 --retry 2 --retry-all-errors --retry-delay 5 \
                -o "$file.part" "$source_url"; then
                downloaded=true
                break
            fi
        elif command -v wget >/dev/null 2>&1; then
            if wget -q --connect-timeout=20 --tries=3 --waitretry=5 \
                -O "$file.part" "$source_url"; then
                downloaded=true
                break
            fi
        else
            echo "FAIL: need curl or wget to fetch $url" >&2
            exit 1
        fi
    done
    if [ "$downloaded" != true ]; then
        rm -f "$file.part"
        echo "FAIL: could not download $url from any configured source" >&2
        exit 1
    fi
    local got
    got=$(sha256_of "$file.part")
    if [ "$got" != "$sha" ]; then
        echo "FAIL: $file sha256 mismatch" >&2
        echo "      want $sha" >&2
        echo "      got  $got" >&2
        rm -f "$file.part"
        exit 1
    fi
    mv "$file.part" "$file"
    echo "ok: $file (downloaded, hash verified)"
}

fetch_one "$KERNEL_URL" "$KERNEL_SHA256"
fetch_one "$BUSYBOX_URL" "$BUSYBOX_SHA256" \
    "https://ftp.gwdg.de/pub/linux/gentoo/distfiles/e3/busybox-1.38.0.tar.bz2"
fetch_one "$MUSL_URL" "$MUSL_SHA256"
fetch_one "$RUNC_X86_64_URL" "$RUNC_X86_64_SHA256"
fetch_one "$RUNC_SOURCE_URL" "$RUNC_SOURCE_SHA256"
fetch_one "$GO_BOOTSTRAP_URL" "$GO_BOOTSTRAP_SHA256"

echo "PASS: platform sources and build inputs are ready in $PWD/dl"
