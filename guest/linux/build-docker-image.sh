#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **Postgres-in-Docker workload initramfs** (task 38, consonance
# workload stream step 3 of 3 — the credibility money-shot: an off-the-shelf
# `docker run --network none postgres` runs deterministically in the guest).
#
# The rootfs is a static busybox + Docker's **static** binary bundle (dockerd +
# containerd + containerd-shim-runc-v2 + runc + the docker CLI + ctr — all
# statically linked, so unlike task 37 there is NO glibc/postgres closure to
# copy: the container ships its own userland inside the baked image) + the
# **official `postgres` image** baked in as a `docker load`-able tar (no runtime
# registry pull) + the `docker-init.sh` /init. The companion kernel is the
# *unchanged* task-36 container-class bzImage (the §capability audit in
# guest/linux/IMPLEMENTATION.md confirmed cgroup-v2, OVERLAY_FS, the namespace
# set, EXT4/LOOP, TMPFS, EPOLL/FUTEX/… are all built in — no kernel change).
#
# **Storage driver: `vfs` on a tmpfs `/var/lib/docker`** — the spec's simplest
# RAM-backed option (just copies layers; space-hungry but we don't care about
# speed, and the box has 100+ GiB). vfs needs no overlay mounts and no ext4
# backing, which is the fewest moving parts for the deterministic bring-up.
# overlay2-on-a-loop-ext4 is the documented alternative (see IMPLEMENTATION.md);
# flip $STORAGE_DRIVER + the daemon flags in docker-init.sh to switch.
#
# Linux + root only (mounts, cgroup, the static-bin layout assume a Linux build
# host; the box is the pinned build environment). On macOS run it in a
# linux/amd64 container as root — see docs/BUILDING.md.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2 cpio gunzip

if [ "$(id -u)" != "0" ]; then
    echo "FAIL: build-docker-image.sh must run as root (the cpio is packed owner 0:0" >&2
    echo "      and the static layout mirrors the privileged guest rootfs)." >&2
    exit 1
fi

# --- tunables ----------------------------------------------------------------
DKROOT=$BUILD_ROOT/dk-root                      # the assembled guest rootfs
DK_STAGE=$BUILD_ROOT/dk-stage                   # extracted docker bundle
DOCKER_TGZ=$DL_DIR/$(basename "$DOCKER_TGZ_URL")
PG_IMAGE_TAR=$DL_DIR/postgres-image.tar         # the baked official postgres image
WORKLOAD_N=20                                   # fixed insert/select iterations (== task 37)

# --- 0. verify the pinned inputs ---------------------------------------------
echo "== docker image: verifying pinned inputs"
if [ ! -f "$DOCKER_TGZ" ]; then
    echo "FAIL: $DOCKER_TGZ missing — run 'make -C guest fetch' first" >&2
    exit 1
fi
got=$(sha256_of "$DOCKER_TGZ")
if [ "$got" != "$DOCKER_TGZ_SHA256" ]; then
    echo "FAIL: $DOCKER_TGZ sha256 mismatch (want $DOCKER_TGZ_SHA256, got $got)" >&2
    exit 1
fi
if [ ! -f "$PG_IMAGE_TAR" ] || [ ! -s "$PG_IMAGE_TAR" ]; then
    echo "FAIL: $PG_IMAGE_TAR missing/empty — run 'make -C guest fetch' on the box" >&2
    echo "      (needs ctr+network; integrity is anchored by the pinned digest in" >&2
    echo "      versions.lock: $POSTGRES_IMAGE_INDEX_DIGEST)." >&2
    exit 1
fi

# --- 1. extract the static docker bundle -------------------------------------
echo "== docker image: extracting docker $DOCKER_VERSION static bundle"
rm -rf "$DK_STAGE"
mkdir -p "$DK_STAGE" "$ART_DIR"
tar -xzf "$DOCKER_TGZ" -C "$DK_STAGE"          # -> $DK_STAGE/docker/<binaries>
[ -x "$DK_STAGE/docker/dockerd" ] || { echo "FAIL: dockerd not in the bundle" >&2; exit 1; }

# --- 2. static busybox (mirrors build-postgres-image.sh; self-contained) -----
echo "== docker image: building static busybox ($BUSYBOX_VERSION)"
extract_busybox
mkdir -p "$BBOBJ"
make -C "$BBSRC" O="$BBOBJ" defconfig >/dev/null
sed -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
    -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    "$BBOBJ/.config" >"$BBOBJ/.config.tmp"
mv "$BBOBJ/.config.tmp" "$BBOBJ/.config"
set +o pipefail            # yes(1) dies of SIGPIPE — judge by make's status alone
yes '' | make -C "$BBSRC" O="$BBOBJ" oldconfig >/dev/null
set -o pipefail
grep -qxF 'CONFIG_STATIC=y' "$BBOBJ/.config" || { echo "FAIL: busybox not static" >&2; exit 1; }
make -C "$BBSRC" O="$BBOBJ" -j"$(nproc)" busybox >/dev/null

# --- 3. assemble the guest rootfs --------------------------------------------
echo "== docker image: assembling rootfs"
rm -rf "$DKROOT"
mkdir -p "$DKROOT"/{bin,sbin,etc,proc,sys,dev,tmp,root}
mkdir -p "$DKROOT/usr/local/bin" "$DKROOT/run" "$DKROOT/var/lib/docker" \
         "$DKROOT/etc/docker" "$DKROOT/sys/fs/cgroup"
ln -sf /run "$DKROOT/var/run"                  # standard /var/run -> /run

cp "$BBOBJ/busybox" "$DKROOT/bin/busybox"
# /bin/sh is the /init interpreter; the rest let any `sh -c` / PATH lookup the
# init (or a sub-shell) does resolve without a full coreutils.
for a in sh mount umount mkdir chmod chown cat echo grep sleep kill nice ln rm cp \
         true false test sync reboot poweroff head tail env printf cut wc ps sed \
         cmp ls id mv touch dd find xargs; do
    ln -sf busybox "$DKROOT/bin/$a"
done

# The static docker stack (dockerd finds containerd/runc/shim via PATH).
for b in dockerd docker containerd containerd-shim-runc-v2 runc ctr docker-proxy docker-init; do
    [ -f "$DK_STAGE/docker/$b" ] && cp "$DK_STAGE/docker/$b" "$DKROOT/usr/local/bin/$b"
done

# Minimal /etc. Static Go uses pure-Go nss (reads these files directly); root is
# all the container stack needs (the postgres user lives inside the image).
printf 'root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/bin/sh\n' >"$DKROOT/etc/passwd"
printf 'root:x:0:\nnobody:x:65534:\n' >"$DKROOT/etc/group"
printf 'passwd: files\ngroup: files\nhosts: files\n' >"$DKROOT/etc/nsswitch.conf"
printf '127.0.0.1 localhost\n::1 localhost\n' >"$DKROOT/etc/hosts"
: >"$DKROOT/etc/resolv.conf"                   # empty — `--network none`, no DNS

# --- 4. bake the official postgres image + the workload ----------------------
echo "== docker image: baking the official postgres image ($(du -h "$PG_IMAGE_TAR" | cut -f1))"
cp "$PG_IMAGE_TAR" "$DKROOT/postgres-image.tar"

# The SAME fixed insert/select workload as task 37: CREATE then N autocommit
# INSERT+SELECT iterations, each reporting the row plus a running count/sum. The
# values are a pure function of the loop index (no wall-clock / random columns),
# so the golden is a deterministic function of the seed. Driven inside the
# container over its local unix socket via `docker exec` (see docker-init.sh).
{
    echo "CREATE TABLE ledger(i int primary key, v bigint);"
    i=1
    while [ "$i" -le "$WORKLOAD_N" ]; do
        echo "INSERT INTO ledger(i,v) VALUES ($i, $i::bigint*$i + 7);"
        echo "SELECT 'row', i, v, (SELECT count(*) FROM ledger), (SELECT sum(v) FROM ledger) FROM ledger WHERE i=$i;"
        i=$((i+1))
    done
} >"$DKROOT/workload.sql"

install -m 0755 "$LINUX_DIR/docker-init.sh" "$DKROOT/init"

# --- 5. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
# DEVTMPFS_MOUNT gives the guest /dev (incl. /dev/console) before init runs.
# Best-effort reproducible; the baked postgres image (a content-addressed
# registry export) is the one large non-byte-reproducible-across-tools input.
echo "== docker image: packing initramfs"
find "$DKROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$DKROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-docker.cpio.gz"
echo "ok: $ART_DIR/initramfs-docker.cpio.gz ($(du -h "$ART_DIR/initramfs-docker.cpio.gz" | cut -f1))"
