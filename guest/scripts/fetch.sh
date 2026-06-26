#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Download the pinned kernel + busybox tarballs into guest/dl/ and verify
# their sha256 against guest/linux/versions.lock. Runs on macOS and Linux;
# after a successful fetch no further network access is needed.
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck source=lib.sh disable=SC1091
. scripts/lib.sh

# shellcheck source=../linux/versions.lock disable=SC1091
. linux/versions.lock

mkdir -p dl

fetch_one() {
    url=$1
    sha=$2
    file="dl/$(basename "$url")"
    if [ -f "$file" ] && [ "$(sha256_of "$file")" = "$sha" ]; then
        echo "ok: $file (cached, hash verified)"
        return
    fi
    echo "fetching $url"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$file.part" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$file.part" "$url"
    else
        echo "FAIL: need curl or wget to fetch $url" >&2
        exit 1
    fi
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
fetch_one "$BUSYBOX_URL" "$BUSYBOX_SHA256"
# PostgreSQL .debs for the task-37 bare-Postgres workload image.
fetch_one "$PG_SERVER_DEB_URL" "$PG_SERVER_DEB_SHA256"
fetch_one "$PG_CLIENT_DEB_URL" "$PG_CLIENT_DEB_SHA256"
fetch_one "$PG_LIBPQ_DEB_URL" "$PG_LIBPQ_DEB_SHA256"
