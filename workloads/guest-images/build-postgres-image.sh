#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **bare-Postgres workload OCI layout**: a static busybox + a real
# PostgreSQL 17 install (from the pinned Debian .debs) + a pre-`initdb`'d
# cluster and the workload entrypoint. The platform package owns the kernel,
# mounts, PID 1, and final guest termination; this image contains only the
# application rootfs and its OCI config.
#
# Linux + root only: the cluster is copied with its guest postgres ownership
# (needs root), `initdb` runs as a non-root build user (postgres refuses uid 0),
# and the runtime shared-library closure is copied from THIS host's /lib (the
# determinism box is the pinned build environment). On macOS run it in a
# linux/amd64 container as root — see CONTRIBUTING.md.
set -euo pipefail

workload_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$workload_dir/../.." && pwd)
cd "$repo_root/consonance/harmony-linux/linux"

# shellcheck source=../../consonance/harmony-linux/linux/lib-build.sh disable=SC1091
. ./lib-build.sh
# shellcheck source=versions.lock disable=SC1091
. "$workload_dir/versions.lock"

require_linux_amd64
require_tools cc make bzip2 dpkg-deb python3 setpriv ldd ldconfig

if [ "$(id -u)" != "0" ]; then
    echo "FAIL: build-postgres-image.sh must run as root (the OCI image preserves uid-70 ownership)." >&2
    exit 1
fi

# --- tunables ----------------------------------------------------------------
PGV=$PG_MAJOR                                  # from versions.lock
PG_UID=70                                      # guest postgres uid/gid (Debian's)
BUILD_UID=65534                                # non-root uid for the build-time initdb
WORKLOAD_N=20                                   # fixed insert/select iterations

PGROOT=$BUILD_ROOT/pg-root                      # the assembled guest rootfs
PG_STAGE=$BUILD_ROOT/pg-stage                   # extracted .debs
STAGEFS=$BUILD_ROOT/pg-stagefs                  # initdb output, copied into PGDATA
PGBIN=$PG_STAGE/usr/lib/postgresql/$PGV/bin

# --- 0. fetch-verify + extract the pinned postgres .debs ---------------------
extract_deb() {
    url=$1 sha=$2
    tarball="$DL_DIR/$(basename "$url")"
    if [ ! -f "$tarball" ]; then
        echo "FAIL: $tarball missing — run 'make -C workloads/guest-images fetch' first" >&2
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

# --- 1. static busybox (mirrors the platform recipe; self-contained here) -----
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
mkdir -p "$PGROOT"/{bin,lib,lib64,etc,proc,sys,dev,tmp,run,var/lib/postgresql}
mkdir -p "$PGROOT/lib/x86_64-linux-gnu" "$PGROOT/usr/lib/x86_64-linux-gnu"
mkdir -p "$PGROOT/usr/local/bin"
install_libvoidstar "$PGROOT"

cp "$BBOBJ/busybox" "$PGROOT/bin/busybox"
# The workload entrypoint uses only this generic BusyBox surface; platform
# mounts and lifecycle operations are deliberately outside the image.
for a in sh mkdir chown chmod sleep printf seq setuidgid cat echo ls \
         head tee env ln rm cp true false test expr sync id; do
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

printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:%s:%s:postgres:/var/lib/postgresql:/bin/sh\n' "$PG_UID" "$PG_UID" >"$PGROOT/etc/passwd"
printf 'root:x:0:\npostgres:x:%s:\n' "$PG_UID" >"$PGROOT/etc/group"
printf 'passwd: files\ngroup: files\n' >"$PGROOT/etc/nsswitch.conf"
ldconfig -r "$PGROOT" 2>/dev/null || true   # ld.so.cache for deterministic lib resolution

# --- 3. bake PGDATA: initdb ONCE at build time into the OCI rootfs ------------
# The data directory keeps initdb's 0700 + uid-70 (postgres requires both).
# initdb runs as a non-root build user (it refuses uid 0); the cluster system identifier it mints from time/pid/
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

# --- determinism overlay (see consonance/harmony-linux/linux/README.md) ---
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

echo "== postgres image: copying the deterministic cluster into PGDATA"
chown -R "$PG_UID:$PG_UID" "$STAGEFS"
mkdir -p "$PGROOT/var/lib/postgresql"
cp -a "$STAGEFS/pgdata" "$PGROOT/var/lib/postgresql/data"
chown -R "$PG_UID:$PG_UID" "$PGROOT/var/lib/postgresql/data"
chmod 0700 "$PGROOT/var/lib/postgresql/data"

# --- 4. the baked workload v2: UUID + wall-clock, still deterministic -
# Each row carries a gen_random_uuid() id (column DEFAULT) and a clock_timestamp()
# wall-clock column. These LOOK nondeterministic — a random UUID, a per-call
# wall-clock time — but must come out BIT-IDENTICAL across two same-seed runs:
# gen_random_uuid() draws from pg_strong_random → the seeded CRNG (the same path
# verified above), and clock_timestamp() reads the system clock, which is
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

install -m 0755 "$workload_dir/postgres-workload.sh" \
    "$PGROOT/usr/local/bin/postgres-workload.sh"

# --- 5. pack the OCI layout --------------------------------------------------
OCI_OUT=$ART_DIR/oci-images/postgres.oci
rm -rf "$OCI_OUT"
mkdir -p "$(dirname "$OCI_OUT")"
python3 "$workload_dir/oci-package.py" \
    --rootfs "$PGROOT" --architecture amd64 --output "$OCI_OUT" \
    --entrypoint /usr/local/bin/postgres-workload.sh \
    --env HARMONY_POSTGRES_VARIANT=postgres \
    --user 0:0 --working-dir /var/lib/postgresql
echo "ok: $OCI_OUT"
