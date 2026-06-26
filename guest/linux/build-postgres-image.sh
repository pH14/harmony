#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **bare-Postgres workload initramfs** (task 37): a static busybox + a
# real PostgreSQL 17 install (from the pinned Debian .debs) + a RAM-backed ext4
# image holding a pre-`initdb`'d cluster + the `pg-init.sh` /init that drives a
# fixed insert/select workload loop. The companion kernel is the *unchanged*
# task-36 container-class bzImage (this task needs no kernel change — ext4, loop,
# brd, tmpfs, AF_UNIX, SysV-IPC are all already built in; see the §capability
# audit in guest/linux/IMPLEMENTATION.md). See that file for the determinism
# closure (locale/TZ pinning, pre-baked PGDATA, pg_strong_random → seeded CRNG).
#
# Linux + root only: `mke2fs -d` bakes the cluster owned by the guest postgres uid
# (needs root), `initdb` runs as a non-root build user (postgres refuses uid 0),
# and the runtime shared-library closure is copied from THIS host's /lib (the
# determinism box is the pinned build environment). On macOS run it in a
# linux/amd64 container as root — see docs/BUILDING.md.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2 cpio dpkg-deb mke2fs setpriv ldd ldconfig

if [ "$(id -u)" != "0" ]; then
    echo "FAIL: build-postgres-image.sh must run as root (mke2fs -d bakes uid-70 ownership)." >&2
    exit 1
fi

# --- tunables ----------------------------------------------------------------
PGV=$PG_MAJOR                                  # from versions.lock
PG_UID=70                                      # guest postgres uid/gid (Debian's)
BUILD_UID=65534                                # non-root uid for the build-time initdb
FIXED_UUID="deadbeef-0000-0000-0000-000000000037"   # pinned ext4 UUID (determinism)
EXT4_SIZE=96M                                   # cluster (~22M) + workload WAL headroom
WORKLOAD_N=20                                   # fixed insert/select iterations

PGROOT=$BUILD_ROOT/pg-root                      # the assembled guest rootfs
PG_STAGE=$BUILD_ROOT/pg-stage                   # extracted .debs
STAGEFS=$BUILD_ROOT/pg-stagefs                  # initdb output, baked into the ext4
PGBIN=$PG_STAGE/usr/lib/postgresql/$PGV/bin

# --- 0. fetch-verify + extract the pinned postgres .debs ---------------------
extract_deb() {
    url=$1 sha=$2
    tarball="$DL_DIR/$(basename "$url")"
    if [ ! -f "$tarball" ]; then
        echo "FAIL: $tarball missing — run 'make -C guest fetch' first" >&2
        exit 1
    fi
    got=$(sha256_of "$tarball")
    if [ "$got" != "$sha" ]; then
        echo "FAIL: $tarball sha256 mismatch (want $sha, got $got)" >&2
        exit 1
    fi
    dpkg-deb -x "$tarball" "$PG_STAGE"
}
echo "== postgres image: extracting pinned PostgreSQL $PG_VERSION .debs"
rm -rf "$PG_STAGE"
mkdir -p "$PG_STAGE" "$ART_DIR"
extract_deb "$PG_SERVER_DEB_URL" "$PG_SERVER_DEB_SHA256"
extract_deb "$PG_CLIENT_DEB_URL" "$PG_CLIENT_DEB_SHA256"
extract_deb "$PG_LIBPQ_DEB_URL"  "$PG_LIBPQ_DEB_SHA256"
[ -x "$PGBIN/postgres" ] || { echo "FAIL: postgres binary not in the extracted deb" >&2; exit 1; }

# --- 1. static busybox (mirrors build-initramfs.sh; self-contained here) ------
echo "== postgres image: building static busybox ($BUSYBOX_VERSION)"
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

# --- 2. assemble the guest rootfs --------------------------------------------
echo "== postgres image: assembling rootfs"
rm -rf "$PGROOT"
mkdir -p "$PGROOT"/{bin,lib,lib64,etc,proc,sys,dev,tmp,run,pgmnt}
mkdir -p "$PGROOT/lib/x86_64-linux-gnu" "$PGROOT/usr/lib/x86_64-linux-gnu"

cp "$BBOBJ/busybox" "$PGROOT/bin/busybox"
for a in sh mount umount mkdir chown chmod sleep printf seq setuidgid cat echo ls \
         head tee env losetup poweroff reboot ln rm cp true false test expr sync id; do
    ln -sf busybox "$PGROOT/bin/$a"
done

# dynamic loader + the shared-lib closure of the binaries the guest runs, plus
# libnss_files (glibc dlopen's it for the /etc/passwd lookup postgres does).
cp -L /lib64/ld-linux-x86-64.so.2 "$PGROOT/lib64/"
export LD_LIBRARY_PATH="$PG_STAGE/usr/lib/postgresql/$PGV/lib:$PG_STAGE/usr/lib/x86_64-linux-gnu"
{ ldd "$PGBIN/postgres"; ldd "$PGBIN/psql"; ldd "$PGBIN/pg_ctl"; } 2>/dev/null \
    | awk '/=> \// {print $3}' | sort -u >"$BUILD_ROOT/pg-libs.txt"
echo /lib/x86_64-linux-gnu/libnss_files.so.2 >>"$BUILD_ROOT/pg-libs.txt"
while read -r so; do
    [ -e "$so" ] && cp -L "$so" "$PGROOT/lib/x86_64-linux-gnu/$(basename "$so")"
done <"$BUILD_ROOT/pg-libs.txt"

# postgres install tree (relocatable — keep Debian's relative bin/lib/share layout)
mkdir -p "$PGROOT/usr/lib/postgresql" "$PGROOT/usr/share/postgresql"
cp -a "$PG_STAGE/usr/lib/postgresql/$PGV" "$PGROOT/usr/lib/postgresql/"
cp -a "$PG_STAGE/usr/share/postgresql/$PGV" "$PGROOT/usr/share/postgresql/"
cp -a "$PG_STAGE/usr/lib/x86_64-linux-gnu/." "$PGROOT/usr/lib/x86_64-linux-gnu/" 2>/dev/null || true
rm -rf "$PGROOT/usr/lib/postgresql/$PGV/lib/bitcode"   # jit=off → no LLVM bitcode

# Debian's postgres is built --with-system-tzdata: ship the zoneinfo DB. glibc's
# C.UTF-8 is file-backed here (not built-in): ship the locale archive + dir.
mkdir -p "$PGROOT/usr/share" "$PGROOT/usr/lib/locale"
cp -a /usr/share/zoneinfo "$PGROOT/usr/share/"
cp -a /usr/lib/locale/locale-archive /usr/lib/locale/C.utf8 "$PGROOT/usr/lib/locale/"

printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:%s:%s:postgres:/pgmnt:/bin/sh\n' "$PG_UID" "$PG_UID" >"$PGROOT/etc/passwd"
printf 'root:x:0:\npostgres:x:%s:\n' "$PG_UID" >"$PGROOT/etc/group"
printf 'passwd: files\ngroup: files\n' >"$PGROOT/etc/nsswitch.conf"
ldconfig -r "$PGROOT" 2>/dev/null || true   # ld.so.cache for deterministic lib resolution

# --- 3. bake PGDATA: initdb ONCE at build time, into a subdir of the ext4 -----
# A subdir keeps initdb's 0700 + uid-70 (postgres requires them of PGDATA; the
# ext4 root that mke2fs creates is root-owned). initdb runs as a non-root build
# user (it refuses uid 0); the cluster system identifier it mints from time/pid/
# random is snapshotted here, so there is no initdb-time nondeterminism at runtime.
echo "== postgres image: initdb (build-time, once) + determinism overlay"
rm -rf "$STAGEFS"
mkdir -p "$STAGEFS/pgdata"
chown -R "$BUILD_UID:$BUILD_UID" "$STAGEFS"
setpriv --reuid="$BUILD_UID" --regid="$BUILD_UID" --clear-groups env LC_ALL=C.UTF-8 TZ=UTC \
    "$PGBIN/initdb" -D "$STAGEFS/pgdata" \
    --locale-provider=libc --locale=C.UTF-8 --encoding=UTF8 \
    -A trust -U postgres -N >"$BUILD_ROOT/initdb.log" 2>&1 \
    || { cat "$BUILD_ROOT/initdb.log"; exit 1; }
cat >>"$STAGEFS/pgdata/postgresql.conf" <<EOF

# --- task 37 determinism overlay (see guest/linux/IMPLEMENTATION.md) ---
listen_addresses = ''            # unix socket only — no networking nondeterminism
unix_socket_directories = '/tmp'
fsync = on                       # exercised; instant + deterministic on RAM storage
jit = off                        # no LLVM bitcode / runtime codegen variability
log_timezone = 'UTC'
timezone = 'UTC'
log_line_prefix = '[pg %p] '     # pid is deterministic (sequential forks); no clock
log_statement = 'none'
shared_buffers = 32MB
max_connections = 16
autovacuum = off                 # keep the short run bounded + the golden clean
max_wal_size = 64MB
EOF

echo "== postgres image: baking fixed-UUID ext4 with the cluster"
chown -R "$PG_UID:$PG_UID" "$STAGEFS"
EXT4=$PGROOT/pgdata.ext4
rm -f "$EXT4"
# Pin the determinism knobs at mkfs once: fixed UUID, and lazy_*_init=0 so there
# is NO background ext4 inode/journal initialization thread firing at runtime.
mke2fs -q -t ext4 -U "$FIXED_UUID" \
    -E lazy_itable_init=0,lazy_journal_init=0 \
    -d "$STAGEFS" -F "$EXT4" "$EXT4_SIZE"

# --- 4. the baked workload v2 (task 42): UUID + wall-clock, still deterministic -
# Each row carries a gen_random_uuid() id (column DEFAULT) and a clock_timestamp()
# wall-clock column. These LOOK nondeterministic — a random UUID, a per-call
# wall-clock time — but must come out BIT-IDENTICAL across two same-seed runs:
# gen_random_uuid() draws from pg_strong_random → the seeded CRNG (the same path
# task 37 verified), and clock_timestamp() reads the system clock, which is
# V-time-driven. Each iteration INSERTs (i, clock_timestamp()) and SELECTs the row
# back with the running count(*)/sum(i) aggregate plus its id + t, streamed as
# `row|i|count|sum|uuid|t`. The count/sum prefix stays a pure function of the loop
# index — the deterministic anchor the gate matches (`row|20|20|210|…`) — while the
# uuid + t are seed-derived (deterministic but not predictable, so the gate checks
# them by *shape* and proves seed-sensitivity at a different seed). gen_random_uuid()
# is built into PostgreSQL core since v13 (PG17 here), so no CREATE EXTENSION pgcrypto
# is needed — confirmed by the workload running clean under ON_ERROR_STOP=1.
{
    echo "CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);"
    i=1
    while [ "$i" -le "$WORKLOAD_N" ]; do
        echo "INSERT INTO ledger(i,t) VALUES ($i, clock_timestamp());"
        echo "SELECT 'row', i, (SELECT count(*) FROM ledger), (SELECT sum(i) FROM ledger), id, t FROM ledger WHERE i=$i;"
        i=$((i+1))
    done
} >"$PGROOT/workload.sql"

install -m 0755 "$LINUX_DIR/pg-init.sh" "$PGROOT/init"

# --- 5. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
# DEVTMPFS_MOUNT gives the guest /dev (incl. /dev/console) before init runs, so no
# device nodes are baked. Best-effort reproducible; the cluster system identifier
# (a build-time event) is the one non-reproducible byte across separate builds.
echo "== postgres image: packing initramfs"
find "$PGROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$PGROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-postgres.cpio.gz"
echo "ok: $ART_DIR/initramfs-postgres.cpio.gz ($(du -h "$ART_DIR/initramfs-postgres.cpio.gz" | cut -f1))"
