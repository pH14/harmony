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

# --- task 86: the commit-pinned libretro NES core (SMB game workload) --------
# The SMB ROM itself is NEVER fetched by any script in this repo (task 86's
# hard requirement) — only the open-source emulator core is pinned here; the
# ROM enters the image build via the user-supplied HARMONY_SMB_ROM path.
fetch_one "$QUICKNES_URL" "$QUICKNES_SHA256"

# --- task 49: k3s (lightweight Kubernetes) -----------------------------------
# The k3s binary + the air-gap images tarball, both URL+sha256-pinned in
# versions.lock (verified against the release's own sha256sum-amd64.txt).
fetch_one "$K3S_BIN_URL" "$K3S_BIN_SHA256"
fetch_one "$K3S_AIRGAP_URL" "$K3S_AIRGAP_SHA256"

# Extract ONLY the pause/sandbox image from the air-gap tarball into a clean
# single-image tar (guest/dl/k3s-pause-image.tar). Every pod needs the sandbox
# container; we --disable coredns/traefik/servicelb/metrics/local-path, so pause
# is the only air-gap image the guest actually runs. Importing just it (a few
# hundred KB) instead of the whole multi-hundred-MB tarball keeps the guest light
# — boot V-time under the single-stepping VMM is the bottleneck. Needs `ctr`
# (containerd), so it is box/Linux-only, like fetch_postgres_image above;
# build-k3s-image.sh fails loudly if the tar is missing.
fetch_k3s_pause_image() {
    out="dl/k3s-pause-image.tar"
    air="dl/$(basename "$K3S_AIRGAP_URL")"
    if [ -f "$out" ] && [ -s "$out" ]; then
        echo "ok: $out (cached; extracted from the digest-pinned air-gap tarball)"
        return
    fi
    if ! command -v ctr >/dev/null 2>&1; then
        echo "skip: $out — needs 'ctr' (containerd). Run 'make -C guest fetch' on the" >&2
        echo "      Linux box; the air-gap tarball is sha256-pinned so the pause image" >&2
        echo "      is content-verified there." >&2
        return
    fi
    ns="ht49-fetch"   # isolated containerd namespace; pruned after export
    echo "importing $air via ctr to extract the pause image (-> $out)"
    ctr -n "$ns" image import "$air" >/dev/null
    # The pause/sandbox image is the only one we keep; find its ref by name (k3s
    # ships it as docker.io/rancher/mirrored-pause:<tag>).
    pause_ref=$(ctr -n "$ns" image ls -q | grep -E 'mirrored-pause|/pause:' | head -1)
    [ -n "$pause_ref" ] || { echo "FAIL: no pause image in $air" >&2; exit 1; }
    echo "   pause image: $pause_ref"
    ctr -n "$ns" image export --platform linux/amd64 "$out.part" "$pause_ref"
    mv "$out.part" "$out"
    # Leave the isolated namespace clean (don't perturb the box's default ns).
    for r in $(ctr -n "$ns" image ls -q); do ctr -n "$ns" image rm "$r" >/dev/null 2>&1 || true; done
    ctr -n "$ns" content prune references >/dev/null 2>&1 || true
    echo "ok: $out ($(sha256_of "$out") — pause image from the digest-pinned air-gap tarball)"
}
fetch_k3s_pause_image
