#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **bug-3 (rare-entropy-value) benchmark initramfs** (task 69 M2): the
# task-37 bare-Postgres image (a static busybox + a real PostgreSQL 17 install +
# a pre-`initdb`'d RAM-backed ext4 cluster) plus the planted-bug supervisor
# `uuid-super` and the `uuid-init.sh` /init that runs it. A verbatim clone of
# build-campaign-image.sh (bug 1) with the supervisor/init/ext4-UUID/output
# swapped, so all three benchmark images share one determinism closure. Produces
# harmony-linux/build/initramfs-uuid.cpio.gz.
#
# This is `build-postgres-image.sh` with two additions, kept in lockstep with it:
#   1. a static `uuid-super` compiled from uuid-super.c and installed at
#      /uuid-super (the supervised process carrying the planted bug), and
#   2. uuid-init.sh installed as /init (postgres workload → supervisor).
# Everything else — the pinned .debs, the determinism overlay, the fixed-UUID
# ext4, the reproducible cpio packing — is identical to the postgres image, so
# the uuid image inherits task 37's determinism closure verbatim.
#
# The companion kernel is the *unchanged* task-36 container-class bzImage (no
# kernel change: mmap/mlock/ioperm(CONFIG_X86_IOPL_IOPERM, default y)/DEVPORT are
# all already available — the foreman verifies these on the box; if ioperm is
# absent, uuid-super's /dev/port fallback path is used instead).
#
# Linux + root only (as build-postgres-image.sh — mke2fs -d bakes uid-70
# ownership). On macOS run it in a linux/amd64 container as root; see
# docs/BUILDING.md.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2 cpio dpkg-deb mke2fs setpriv ldd ldconfig

if [ "$(id -u)" != "0" ]; then
    echo "FAIL: build-uuid-image.sh must run as root (mke2fs -d bakes uid-70 ownership)." >&2
    exit 1
fi

# --- tunables (mirror build-postgres-image.sh; distinct ext4 UUID) -----------
PGV=$PG_MAJOR                                  # from versions.lock
PG_UID=70                                      # guest postgres uid/gid (Debian's)
BUILD_UID=65534                                # non-root uid for the build-time initdb
FIXED_UUID="deadbeef-0000-0000-0000-000000000063"   # pinned ext4 UUID (bug 3, distinct)
EXT4_SIZE=96M                                   # cluster (~22M) + workload WAL headroom
WORKLOAD_N=20                                   # fixed insert/select iterations

PGROOT=$BUILD_ROOT/uuid-root                # the assembled guest rootfs
PG_STAGE=$BUILD_ROOT/uuid-stage             # extracted .debs
STAGEFS=$BUILD_ROOT/uuid-stagefs            # initdb output, baked into the ext4
PGBIN=$PG_STAGE/usr/lib/postgresql/$PGV/bin

# --- 0. fetch-verify + extract the pinned postgres .debs ---------------------
extract_deb() {
    url=$1 sha=$2
    tarball="$DL_DIR/$(basename "$url")"
    if [ ! -f "$tarball" ]; then
        echo "FAIL: $tarball missing — run 'make -C harmony-linux fetch' first" >&2
        exit 1
    fi
    got=$(sha256_of "$tarball")
    if [ "$got" != "$sha" ]; then
        echo "FAIL: $tarball sha256 mismatch (want $sha, got $got)" >&2
        exit 1
    fi
    dpkg-deb -x "$tarball" "$PG_STAGE"
}
echo "== uuid image: extracting pinned PostgreSQL $PG_VERSION .debs"
rm -rf "$PG_STAGE"
mkdir -p "$PG_STAGE" "$ART_DIR"
extract_deb "$PG_SERVER_DEB_URL" "$PG_SERVER_DEB_SHA256"
extract_deb "$PG_CLIENT_DEB_URL" "$PG_CLIENT_DEB_SHA256"
extract_deb "$PG_LIBPQ_DEB_URL"  "$PG_LIBPQ_DEB_SHA256"
[ -x "$PGBIN/postgres" ] || { echo "FAIL: postgres binary not in the extracted deb" >&2; exit 1; }

# --- 1. static busybox (mirrors build-postgres-image.sh) ---------------------
echo "== uuid image: building static busybox ($BUSYBOX_VERSION)"
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

# --- 1b. the planted-bug supervisor: static uuid-super -------------------
# Static (no shared-lib closure to manage) and -O2 for a compact, deterministic
# loop. SOURCE_DATE_EPOCH + a fixed build id keep it reproducible across builds.
echo "== uuid image: compiling static uuid-super"
cc -static -O2 -Wall -Wextra -fno-asynchronous-unwind-tables \
    -o "$BUILD_ROOT/uuid-super" uuid-super.c
[ -x "$BUILD_ROOT/uuid-super" ] || { echo "FAIL: uuid-super did not build" >&2; exit 1; }

# --- 2. assemble the guest rootfs (mirrors build-postgres-image.sh) ----------
echo "== uuid image: assembling rootfs"
rm -rf "$PGROOT"
mkdir -p "$PGROOT"/{bin,lib,lib64,etc,proc,sys,dev,tmp,run,pgmnt}
mkdir -p "$PGROOT/lib/x86_64-linux-gnu" "$PGROOT/usr/lib/x86_64-linux-gnu"
install_libvoidstar "$PGROOT"

cp "$BBOBJ/busybox" "$PGROOT/bin/busybox"
for a in sh mount umount mkdir chown chmod sleep printf seq setuidgid cat echo ls \
         head tee env losetup poweroff reboot ln rm cp true false test expr sync id; do
    ln -sf busybox "$PGROOT/bin/$a"
done

# The static supervisor: no lib closure needed.
cp "$BUILD_ROOT/uuid-super" "$PGROOT/uuid-super"
chmod 0755 "$PGROOT/uuid-super"

# dynamic loader + the shared-lib closure of the postgres binaries (as
# build-postgres-image.sh).
cp -L /lib64/ld-linux-x86-64.so.2 "$PGROOT/lib64/"
export LD_LIBRARY_PATH="$PG_STAGE/usr/lib/postgresql/$PGV/lib:$PG_STAGE/usr/lib/x86_64-linux-gnu"
{ ldd "$PGBIN/postgres"; ldd "$PGBIN/psql"; ldd "$PGBIN/pg_ctl"; } 2>/dev/null \
    | awk '/=> \// {print $3}' | sort -u >"$BUILD_ROOT/uuid-libs.txt"
echo /lib/x86_64-linux-gnu/libnss_files.so.2 >>"$BUILD_ROOT/uuid-libs.txt"
while read -r so; do
    [ -e "$so" ] && cp -L "$so" "$PGROOT/lib/x86_64-linux-gnu/$(basename "$so")"
done <"$BUILD_ROOT/uuid-libs.txt"

# postgres install tree (relocatable — keep Debian's relative layout)
mkdir -p "$PGROOT/usr/lib/postgresql" "$PGROOT/usr/share/postgresql"
cp -a "$PG_STAGE/usr/lib/postgresql/$PGV" "$PGROOT/usr/lib/postgresql/"
cp -a "$PG_STAGE/usr/share/postgresql/$PGV" "$PGROOT/usr/share/postgresql/"
cp -a "$PG_STAGE/usr/lib/x86_64-linux-gnu/." "$PGROOT/usr/lib/x86_64-linux-gnu/" 2>/dev/null || true
rm -rf "$PGROOT/usr/lib/postgresql/$PGV/lib/bitcode"   # jit=off → no LLVM bitcode

mkdir -p "$PGROOT/usr/share" "$PGROOT/usr/lib/locale"
cp -a /usr/share/zoneinfo "$PGROOT/usr/share/"
cp -a /usr/lib/locale/locale-archive /usr/lib/locale/C.utf8 "$PGROOT/usr/lib/locale/"

printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:%s:%s:postgres:/pgmnt:/bin/sh\n' "$PG_UID" "$PG_UID" >"$PGROOT/etc/passwd"
printf 'root:x:0:\npostgres:x:%s:\n' "$PG_UID" >"$PGROOT/etc/group"
printf 'passwd: files\ngroup: files\n' >"$PGROOT/etc/nsswitch.conf"
ldconfig -r "$PGROOT" 2>/dev/null || true

# --- 3. bake PGDATA: initdb ONCE at build time (mirrors build-postgres-image.sh) ---
echo "== uuid image: initdb (build-time, once) + determinism overlay"
rm -rf "$STAGEFS"
mkdir -p "$STAGEFS/pgdata"
chown -R "$BUILD_UID:$BUILD_UID" "$STAGEFS"
setpriv --reuid="$BUILD_UID" --regid="$BUILD_UID" --clear-groups env LC_ALL=C.UTF-8 TZ=UTC \
    "$PGBIN/initdb" -D "$STAGEFS/pgdata" \
    --locale-provider=libc --locale=C.UTF-8 --encoding=UTF8 \
    -A trust -U postgres -N >"$BUILD_ROOT/uuid-initdb.log" 2>&1 \
    || { cat "$BUILD_ROOT/uuid-initdb.log"; exit 1; }
cat >>"$STAGEFS/pgdata/postgresql.conf" <<EOF

# --- task 37 determinism overlay (see harmony-linux/linux/IMPLEMENTATION.md) ---
listen_addresses = ''
unix_socket_directories = '/tmp'
fsync = on
jit = off
log_timezone = 'UTC'
timezone = 'UTC'
log_line_prefix = '[pg %p] '
log_statement = 'none'
shared_buffers = 32MB
max_connections = 16
autovacuum = off
max_wal_size = 64MB
EOF

echo "== uuid image: baking fixed-UUID ext4 with the cluster"
chown -R "$PG_UID:$PG_UID" "$STAGEFS"
EXT4=$PGROOT/pgdata.ext4
rm -f "$EXT4"
mke2fs -q -t ext4 -U "$FIXED_UUID" \
    -E lazy_itable_init=0,lazy_journal_init=0 \
    -d "$STAGEFS" -F "$EXT4" "$EXT4_SIZE"

# --- 4. the baked workload v2 (task 42), identical to the postgres image ------
{
    echo "CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);"
    i=1
    while [ "$i" -le "$WORKLOAD_N" ]; do
        echo "INSERT INTO ledger(i,t) VALUES ($i, clock_timestamp());"
        echo "SELECT 'row', i, (SELECT count(*) FROM ledger), (SELECT sum(i) FROM ledger), id, t FROM ledger WHERE i=$i;"
        i=$((i+1))
    done
} >"$PGROOT/workload.sql"

install -m 0755 "$LINUX_DIR/uuid-init.sh" "$PGROOT/init"

# --- 5. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
echo "== uuid image: packing initramfs"
find "$PGROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$PGROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-uuid.cpio.gz"
echo "ok: $ART_DIR/initramfs-uuid.cpio.gz ($(du -h "$ART_DIR/initramfs-uuid.cpio.gz" | cut -f1))"
