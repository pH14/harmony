#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **Postgres-in-Docker workload initramfs** (task 38, consonance
# workload stream step 3 of 3 — the credibility money-shot: the off-the-shelf
# **official `postgres` image** runs deterministically in the guest as a real
# OCI container).
#
# **Why runc-direct, not dockerd (the load-bearing finding — see IMPLEMENTATION.md).**
# We bake the FULL Docker static stack (dockerd + containerd + containerd-shim +
# runc) into the rootfs, but the deterministic run drives the container with
# **`runc` directly** — the same low-level OCI runtime dockerd/containerd invoke
# under the hood. The reason: under consonance's single-vCPU / V-time model
# (V-time advances only at VM-exits), the long-running **dockerd daemon
# busy-spins with no VM-exit** (its Go runtime spin-waits on its containerd over
# gRPC), which freezes V-time → the LAPIC tick never fires → nothing is ever
# scheduled → deadlock. runc avoids this entirely: it is NOT a long-running
# daemon — it sets up the container (namespaces, cgroups, rootfs) and runs to
# completion, so there is no idle daemon to spin. The container it runs is the
# identical official-image container docker would run (`docker run` is just
# dockerd → containerd → runc + image management; we keep the image + runc).
#
# The rootfs is a static busybox + the static Docker bundle + an **OCI bundle**
# (the official postgres image's rootfs, extracted from the registry export, +
# a generated `config.json`). The companion kernel is the *unchanged* task-36
# container-class bzImage (the §capability audit in guest/linux/IMPLEMENTATION.md
# confirmed cgroup-v2, the namespace set, TMPFS, EPOLL/FUTEX/… are all built in —
# no kernel change). The container rootfs lives in the initramfs tmpfs (RAM), so
# PGDATA is RAM-backed (fsync is a noop — no durability-fault surface, deferred
# to D1, as in task 37).
#
# Linux + root only (mounts, cgroup, the static-bin layout assume a Linux build
# host; the box is the pinned build environment). On macOS run it in a
# linux/amd64 container as root — see docs/BUILDING.md.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2 cpio gunzip jq tar chroot mount umount
# `runc` (for `runc spec`) is taken from the docker bundle we extract below.

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
# DEFENSIVE: umount any leaked build-time bind mounts (the §4 initdb binds the
# host /dev/proc into the rootfs) BEFORE rm -rf — `rm -rf` crosses mount points,
# so deleting an un-umounted /dev bind would wipe the host's /dev.
umount "$DKROOT/oci/rootfs/proc" 2>/dev/null || true
umount "$DKROOT/oci/rootfs/dev" 2>/dev/null || true
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

# runc `--no-pivot` wrapper. The container rootfs lives in the initramfs
# (a ramdisk whose root mount has no parent), so runc's default pivot_root
# EINVALs ("pivot_root: invalid argument"); runc's `--no-pivot` switches to the
# MS_MOVE+chroot path that is documented for exactly "rootfs on top of a
# ramdisk". containerd's shim resolves `runc` by PATH, so we shim it: the real
# binary becomes runc.real and `runc` is this wrapper, which injects --no-pivot
# into the create/run subcommand (IFS=newline rebuild → spaces in args survive;
# runc argv has no newlines). Determinism-neutral (a fixed arg).
mv "$DKROOT/usr/local/bin/runc" "$DKROOT/usr/local/bin/runc.real"
cat >"$DKROOT/usr/local/bin/runc" <<'WRAP'
#!/bin/sh
new_args() {
    injected=0
    for a in "$@"; do
        printf '%s\n' "$a"
        if [ "$injected" = 0 ] && { [ "$a" = create ] || [ "$a" = run ]; }; then
            printf '%s\n' --no-pivot
            injected=1
        fi
    done
}
OLDIFS=$IFS
IFS='
'
set -- $(new_args "$@")
IFS=$OLDIFS
exec /usr/local/bin/runc.real "$@"
WRAP
chmod 0755 "$DKROOT/usr/local/bin/runc"

# Minimal /etc. Static Go uses pure-Go nss (reads these files directly); root is
# all the container stack needs (the postgres user lives inside the image).
printf 'root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/bin/sh\n' >"$DKROOT/etc/passwd"
printf 'root:x:0:\nnobody:x:65534:\n' >"$DKROOT/etc/group"
printf 'passwd: files\ngroup: files\nhosts: files\n' >"$DKROOT/etc/nsswitch.conf"
printf '127.0.0.1 localhost\n::1 localhost\n' >"$DKROOT/etc/hosts"
: >"$DKROOT/etc/resolv.conf"                   # empty — `--network none`, no DNS

# --- 4. build the OCI bundle from the official postgres image -----------------
# Extract the image's merged rootfs (apply its layers in order, with best-effort
# whiteout handling) and generate an OCI runtime `config.json` from the image's
# own runtime config (entrypoint/cmd/env/cwd) — the bundle `runc run` consumes.
echo "== docker image: extracting the official postgres image rootfs"
IMG=$BUILD_ROOT/dk-img
rm -rf "$IMG"; mkdir -p "$IMG"
tar -xf "$PG_IMAGE_TAR" -C "$IMG"
MANI=$IMG/manifest.json
[ -f "$MANI" ] || { echo "FAIL: no manifest.json in the image export" >&2; exit 1; }
CFG=$IMG/$(jq -r '.[0].Config' "$MANI")
[ -f "$CFG" ] || { echo "FAIL: image config blob $CFG missing" >&2; exit 1; }

BUNDLE=$DKROOT/oci
mkdir -p "$BUNDLE/rootfs"
# Apply each layer in order. Whiteouts (.wh.<f> = delete, .wh..wh..opq = opaque
# dir) are applied right after the layer that introduces them, approximating
# overlayfs semantics. Stale-but-undeleted files would only bloat the rootfs;
# they do not affect postgres — so this is robust for this image.
for layer in $(jq -r '.[0].Layers[]' "$MANI"); do
    tar -xf "$IMG/$layer" -C "$BUNDLE/rootfs"
    find "$BUNDLE/rootfs" -name '.wh..wh..opq' -delete 2>/dev/null || true
    find "$BUNDLE/rootfs" -name '.wh.*' 2>/dev/null | while read -r wh; do
        target="$(dirname "$wh")/$(basename "$wh" | sed 's/^\.wh\.//')"
        rm -rf "${target:?}" "$wh"      # :? guards against an empty expansion
    done
done
[ -x "$BUNDLE/rootfs/usr/lib/postgresql/$PG_MAJOR/bin/postgres" ] \
    || { echo "FAIL: postgres binary not in the extracted image rootfs" >&2; exit 1; }

# The SAME workload v2 as task 37 (task 42), baked INTO the container rootfs so the
# in-container `psql -f /workload.sql` drives the live DB over its local unix socket:
# each row carries a gen_random_uuid() id (column DEFAULT) + a clock_timestamp()
# wall-clock column. They LOOK nondeterministic but come out BIT-IDENTICAL twice —
# gen_random_uuid() rides pg_strong_random → the seeded CRNG, clock_timestamp() reads
# the V-time clock — now proven through the FULL container surface. The SELECT streams
# `row|i|count|sum|uuid|t`; the count/sum prefix is a pure function of the loop index
# (the deterministic anchor, `row|20|20|210|…`), the uuid + t are seed-derived.
# gen_random_uuid() is built into PostgreSQL core since v13 (the official postgres:17
# image here), so no CREATE EXTENSION is needed.
{
    echo "CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);"
    i=1
    while [ "$i" -le "$WORKLOAD_N" ]; do
        echo "INSERT INTO ledger(i,t) VALUES ($i, clock_timestamp());"
        echo "SELECT 'row', i, (SELECT count(*) FROM ledger), (SELECT sum(i) FROM ledger), id, t FROM ledger WHERE i=$i;"
        i=$((i+1))
    done
} >"$BUNDLE/rootfs/workload.sql"

# The in-container flow script (the container's PID 1): starts postgres, drives
# the cooperative psql loop + workload, stops it — the whole task-37 flow, run
# *inside* the container so it advances V-time under the VMM. See its header.
install -m 0755 "$LINUX_DIR/pg-container-run.sh" "$BUNDLE/rootfs/run-workload.sh"

# Pre-bake PGDATA: run the image's own `initdb` ONCE at build time (as the
# postgres user, uid 999), exactly like task 37 pre-baked its bare cluster — and
# for the SAME reason, now load-bearing for the container path. Running the
# official image's *entrypoint* would `initdb` at container START, which is both
# crushingly slow under the single-stepping VMM AND re-execs through `gosu` (a Go
# program whose runtime busy-spins with no VM-exit → it FREEZES V-time, the same
# failure mode as dockerd). Pre-baking lets us run the `postgres` binary directly
# as PID 1 (no entrypoint, no gosu, no runtime initdb) — a cooperative C workload
# identical to task 37's, now inside an OCI container. `initdb` runs in a `chroot`
# (the image's own binary + libs) with /dev,/proc bind-mounted; the cluster
# system identifier it mints is snapshotted here, so there is no initdb-time
# nondeterminism at runtime.
echo "== docker image: pre-baking PGDATA (build-time initdb as uid 999)"
PGDATA_REL=/var/lib/postgresql/data
PGBIN=/usr/lib/postgresql/$PG_MAJOR/bin
# initdb needs a few device nodes; mknod them directly in the rootfs (NOT a bind
# mount of the host /dev — a leaked bind mount would make a later `rm -rf` delete
# the host's /dev). runc mounts a fresh tmpfs /dev in the container at runtime, so
# these baked nodes are hidden then — harmless.
# Bind the host's working /dev + /proc into the rootfs for initdb (BUILD_ROOT is
# on a `nodev` tmpfs, so mknod'd nodes don't function; initdb needs /dev/null and
# checks /proc). The EXIT trap + the §3 defensive umount guarantee these never
# leak into a later `rm -rf`. runc gives the *runtime* container its own /dev.
mount --bind /dev "$BUNDLE/rootfs/dev"
mount -t proc proc "$BUNDLE/rootfs/proc"
trap 'umount "$BUNDLE/rootfs/proc" 2>/dev/null || true; umount "$BUNDLE/rootfs/dev" 2>/dev/null || true' EXIT
# Drop to the postgres uid with chroot's own --userspec (not gosu, which is a Go
# binary that needs /proc/self/exe). initdb refuses to run as root.
chroot --userspec=999:999 "$BUNDLE/rootfs" /bin/sh -c "
    cd /var/lib/postgresql
    export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC HOME=/var/lib/postgresql
    exec $PGBIN/initdb -D $PGDATA_REL \
        --locale=C.UTF-8 --encoding=UTF8 --auth-local=trust --auth-host=trust -U postgres -N
" >"$BUILD_ROOT/initdb-bundle.log" 2>&1 || { cat "$BUILD_ROOT/initdb-bundle.log"; exit 1; }
umount "$BUNDLE/rootfs/proc" 2>/dev/null || true
umount "$BUNDLE/rootfs/dev" 2>/dev/null || true
trap - EXIT
# Determinism overlay on the baked cluster's postgresql.conf (mirrors task 37):
# socket-only, pinned TZ/locale, deterministic pid log prefix, autovacuum off.
cat >>"$BUNDLE/rootfs$PGDATA_REL/postgresql.conf" <<EOF

# --- task 38 determinism overlay (see guest/linux/IMPLEMENTATION.md) ---
listen_addresses = ''            # unix socket only — no networking nondeterminism
unix_socket_directories = '/run/postgresql'
fsync = on                       # instant + deterministic on RAM-backed rootfs
jit = off
log_timezone = 'UTC'
timezone = 'UTC'
log_line_prefix = '[pg %p] '     # deterministic pid (sequential forks); no clock
log_statement = 'none'
shared_buffers = 32MB
max_connections = 16
autovacuum = off
max_wal_size = 64MB
EOF

echo "== docker image: generating the OCI config.json (postgres direct, uid 999)"
RUNC=$DK_STAGE/docker/runc                      # the real runc (not the wrapper)
"$RUNC" spec --bundle "$BUNDLE"                 # default template -> $BUNDLE/config.json
IMG_ENV=$(jq -c '.config.Env // []' "$CFG")     # the image's PATH/PG_* etc.
# Run the image's postgres binary DIRECTLY as the postgres user (uid 999) on the
# pre-baked cluster — no entrypoint, no gosu, no initdb (see the pre-bake note).
# terminal=false so the container's stdout/stderr are inherited from `runc run`
# (= ttyS0). The default template's namespaces include a fresh empty NETWORK
# namespace = `--network none` (loopback only); the workload reaches postgres over
# the local unix socket. Allow all devices (the bare `runc spec` default-deny
# device cgroup is an eBPF filter that kills PID 1 at exec on the guest kernel) —
# fine for a trusted single-purpose determinism gate.
jq --argjson env "$IMG_ENV" '
    .process.terminal = false
  | .process.args = ["/bin/sh", "/run-workload.sh"]
  | .process.cwd = "/var/lib/postgresql"
  | .process.user = {"uid": 999, "gid": 999, "additionalGids": [999]}
  | .process.env = ($env + ["LC_ALL=C.UTF-8", "LANG=C.UTF-8", "TZ=UTC", "PGTZ=UTC"])
  | .process.noNewPrivileges = false
  | .root.path = "rootfs"
  | .root.readonly = false
  | .linux.cgroupsPath = "pg-container"
  | .linux.resources.devices = [{"allow": true, "access": "rwm"}]
' "$BUNDLE/config.json" >"$BUNDLE/config.json.new"
mv "$BUNDLE/config.json.new" "$BUNDLE/config.json"
echo "   container runs /run-workload.sh as uid 999 on pre-baked PGDATA; rootfs=$(du -sh "$BUNDLE/rootfs" | cut -f1)"

# The guest /init and the in-namespace container-setup helper (the latter runs
# as the unshared container PID 1, before chroot; see docker-init.sh).
install -m 0755 "$LINUX_DIR/docker-init.sh" "$DKROOT/init"
install -m 0755 "$LINUX_DIR/container-setup.sh" "$DKROOT/container-setup.sh"
# Task 48: the REAL-runc /init, baked alongside as /runc-init and selected via the
# kernel `rdinit=/runc-init` cmdline param (the task-38 unshare path above stays the
# default /init for comparison). It `runc run`s the SAME /oci bundle generated above
# — the config.json `runc spec` already wrote is runc-ready (allow-all devices,
# terminal=false, runs /run-workload.sh). The unlock vs task 38: the Go runtime is
# now preempted at the V-time LAPIC deadline (task 47 run_until), so runc's
# container-init no longer deadlocks. See runc-init.sh + tasks/48-runc-postgres.md.
install -m 0755 "$LINUX_DIR/runc-init.sh" "$DKROOT/runc-init"

# --- 5. pack the initramfs (sorted, fixed mtime, gzip -n) ---------------------
# DEVTMPFS_MOUNT gives the guest /dev (incl. /dev/console) before init runs.
# **Ownership is PRESERVED** (no `--owner=0:0`): the guest-side files (busybox,
# the docker bins, /init, /etc) are root-owned because root created them, while
# the OCI bundle keeps the image's own ownerships + the pre-baked PGDATA owned by
# uid 999 — which the container's postgres (uid 999) needs at runtime. Forcing
# 0:0 would make PGDATA root-owned and postgres would refuse it. Ownership is a
# deterministic function of the image + initdb, so this stays reproducible.
echo "== docker image: packing initramfs"
find "$DKROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$DKROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-docker.cpio.gz"
echo "ok: $ART_DIR/initramfs-docker.cpio.gz ($(du -h "$ART_DIR/initramfs-docker.cpio.gz" | cut -f1))"
