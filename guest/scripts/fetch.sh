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
# Docker's static binary bundle for the task-38 Postgres-in-Docker image
# (sha256-pinned, curl-able anywhere).
fetch_one "$DOCKER_TGZ_URL" "$DOCKER_TGZ_SHA256"

# The official postgres image for task 38 — pulled by registry digest with the
# box's `ctr` (containerd) and exported to a `docker load`-able tar. This step
# needs a running containerd + network, so it is Linux/box-only and skipped
# (with a clear note) elsewhere; the task-38 image build is Linux-root-only
# anyway, and build-docker-image.sh fails loudly if the tar is missing.
fetch_postgres_image() {
    out="dl/postgres-image.tar"
    if [ -f "$out" ] && [ -s "$out" ]; then
        echo "ok: $out (cached; integrity anchored by the pinned registry digest)"
        return
    fi
    if ! command -v ctr >/dev/null 2>&1; then
        echo "skip: $out — needs 'ctr' (containerd). Run 'make -C guest fetch' on the" >&2
        echo "      Linux box where the task-38 image is built; the digest is pinned in" >&2
        echo "      versions.lock so the pull is content-verified there." >&2
        return
    fi
    ns="ht38-fetch"   # isolated containerd namespace; pruned after export
    ref="docker.io/library/${POSTGRES_IMAGE_NAME%%:*}@${POSTGRES_IMAGE_INDEX_DIGEST}"
    echo "fetching $ref via ctr (-> $out)"
    ctr -n "$ns" image pull --platform linux/amd64 "$ref"
    # Tag with the human ref so the exported tar carries RepoTags and the guest's
    # `docker load` imports it as ${POSTGRES_IMAGE_NAME} (so `docker run` resolves).
    ctr -n "$ns" image tag "$ref" "docker.io/library/$POSTGRES_IMAGE_NAME" 2>/dev/null || true
    ctr -n "$ns" image export --platform linux/amd64 "$out.part" "docker.io/library/$POSTGRES_IMAGE_NAME"
    mv "$out.part" "$out"
    # Leave the isolated namespace clean (don't perturb the box's default ns).
    ctr -n "$ns" image rm "docker.io/library/$POSTGRES_IMAGE_NAME" "$ref" >/dev/null 2>&1 || true
    ctr -n "$ns" content prune references >/dev/null 2>&1 || true
    echo "ok: $out ($(sha256_of "$out") — derived from the digest-pinned pull)"
}
fetch_postgres_image
