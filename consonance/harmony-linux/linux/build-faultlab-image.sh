#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **fault-library workload initramfs** -> initramfs-faultlab.cpio.gz.
#
# One image carries both historical specimens, selected by `rdinit=`:
#   /etcd-init          etcd 3.5.2, the consistent-index inconsistency
#                       (bugs/historical/etcd-3.5-inconsistency)
#   /etcd-control-init  etcd 3.5.3, that bug's fixed control arm
#   /pgcic-init         PostgreSQL 14.3, the CREATE INDEX CONCURRENTLY race
#                       (bugs/historical/postgres-cic-corruption)
#   /pgcic-control-init PostgreSQL 14.4, that bug's fixed control arm
#   /sqlite-init        SQLite 3.51.2, the WAL-reset checkpoint race
#                       (bugs/historical/sqlite-wal-reset)
#   /sqlite-control-init SQLite 3.51.3, that bug's fixed control arm
#   /ant-sqlite-init    SQLite 3.51.2 with the Antithesis reach markers and
#                       their two-writer workload, edge-counted for pauses
#   /ant-sqlite-control-init the same on SQLite 3.51.3
#
# Sharing one image keeps the two specimens on identical busybox, libvoidstar
# and fault-agent bytes, so a difference between campaigns is a difference in
# the workload and never in the harness.
#
# Linux + root only, for the same reasons as build-postgres-image.sh: `mke2fs -d`
# bakes the cluster owned by the guest postgres uid, `initdb` runs as a non-root
# build user, and the glibc closure is copied from this host's own /lib.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2 cpio mke2fs setpriv ldd ldconfig losetup curl patch

if [ "$(id -u)" != "0" ]; then
    echo "FAIL: build-faultlab-image.sh must run as root (mke2fs -d bakes uid-70 ownership)." >&2
    exit 1
fi

# --- tunables ----------------------------------------------------------------
PG_UID=70                                     # guest postgres uid/gid (Debian's)
BUILD_UID=65534                               # non-root uid for the build-time initdb
# cpio stores a file's apparent size, so the ext4 images are sized to what the
# workload needs and no more: two of them ride in every boot's initramfs, and
# the whole thing is decompressed into guest RAM.
EXT4_SIZE=128M                                # seeded cluster (~50M) + index churn + WAL
SEED_ROWS=20000                               # rows in the churned table
FILLFACTOR=10                                 # room for HOT versions, and many pages per row so a build's scans outlast a churn cycle

ROOT=$BUILD_ROOT/faultlab-root                # the assembled guest rootfs
STAGE=$BUILD_ROOT/faultlab-stage              # extracted/installed upstream trees
AGENT_BIN=$GUEST_DIR/fault-agent/target/x86_64-unknown-linux-musl/release/fault-agent

# ext4 UUIDs are pinned per version so the two clusters can never be confused
# and neither carries a build-time random identifier.
ext4_uuid() { printf 'fa017c4b-0000-0000-0000-%012d\n' "${1//./}"; }

# --- 0. pinned downloads ------------------------------------------------------
# These three are not in scripts/fetch.sh; fetch them here so the image build is
# self-contained, under the same pin-then-verify discipline.
fetch_pinned() {
    url=$1 sha=$2
    file="$DL_DIR/$(basename "$url")"
    if [ ! -f "$file" ] || [ "$(sha256_of "$file")" != "$sha" ]; then
        echo "== faultlab: fetching $(basename "$url")" >&2
        mkdir -p "$DL_DIR"
        curl -fsSL -o "$file.part" "$url"
        got=$(sha256_of "$file.part")
        if [ "$got" != "$sha" ]; then
            echo "FAIL: $(basename "$url") sha256 mismatch (want $sha, got $got)" >&2
            rm -f "$file.part"
            exit 1
        fi
        mv "$file.part" "$file"
    fi
    echo "$file"
}

ETCD_TGZ=$(fetch_pinned "$ETCD_URL" "$ETCD_SHA256")
ETCD_CTL_TGZ=$(fetch_pinned "$ETCD_CONTROL_URL" "$ETCD_CONTROL_SHA256")
SQLITE_TGZ=$(fetch_pinned "$SQLITE_URL" "$SQLITE_SHA256")
SQLITE_CTL_TGZ=$(fetch_pinned "$SQLITE_CONTROL_URL" "$SQLITE_CONTROL_SHA256")
PG_CIC_TAR=$(fetch_pinned "$PG_CIC_URL" "$PG_CIC_SHA256")
PG_CTL_TAR=$(fetch_pinned "$PG_CIC_CONTROL_URL" "$PG_CIC_CONTROL_SHA256")

# --- 1. the fault agent -------------------------------------------------------
# The agent is what turns this image into a searchable target. An image without
# it boots and runs the nominal-control path but can never take a fault, which
# is a silently useless campaign artifact — so refuse to build one.
# The nominal controls drive the hooks straight from init and never call the
# agent, so they can be measured before it exists. That path sets
# FAULTLAB_NO_AGENT=1 and gets a differently named artifact, which is what keeps
# an agent-less image from being picked up as a campaign target by mistake.
ARTIFACT=initramfs-faultlab.cpio.gz
if [ ! -x "$AGENT_BIN" ]; then
    if [ "${FAULTLAB_NO_AGENT:-0}" != "1" ]; then
        echo "FAIL: fault-agent binary not found at" >&2
        echo "      $AGENT_BIN" >&2
        echo "      Build it first:" >&2
        echo "        cargo build --release --target x86_64-unknown-linux-musl \\" >&2
        echo "          --manifest-path consonance/harmony-linux/fault-agent/Cargo.toml" >&2
        echo "      Or set FAULTLAB_NO_AGENT=1 to build the control-only image" >&2
        echo "      (initramfs-faultlab-noagent.cpio.gz), which cannot take a fault." >&2
        exit 1
    fi
    ARTIFACT=initramfs-faultlab-noagent.cpio.gz
    echo "WARNING: building WITHOUT the fault agent -> $ARTIFACT." >&2
    echo "         This image runs the nominal controls only; it can take no fault." >&2
fi

# --- 2. static busybox --------------------------------------------------------
echo "== faultlab: building static busybox ($BUSYBOX_VERSION)"
extract_busybox
prepare_busybox_build_source
mkdir -p "$BBOBJ" "$ART_DIR"
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

# --- 3. upstream trees --------------------------------------------------------
rm -rf "$STAGE"
for pair in "$ETCD_VERSION:$ETCD_TGZ" "$ETCD_CONTROL_VERSION:$ETCD_CTL_TGZ"; do
    v=${pair%%:*} tgz=${pair#*:}
    mkdir -p "$STAGE/etcd-$v"
    echo "== faultlab: extracting etcd $v"
    tar -xzf "$tgz" -C "$STAGE/etcd-$v" --strip-components=1
    if [ ! -x "$STAGE/etcd-$v/etcd" ] || [ ! -x "$STAGE/etcd-$v/etcdctl" ]; then
        echo "FAIL: etcd/etcdctl missing from the $v release tarball" >&2
        exit 1
    fi
done

# The SQLite workload is one static binary per release, compiled against that
# release's amalgamation, so the two arms differ in the SQLite sources alone.
for pair in "$SQLITE_VERSION:$SQLITE_TGZ" "$SQLITE_CONTROL_VERSION:$SQLITE_CTL_TGZ"; do
    v=${pair%%:*} tgz=${pair#*:}
    mkdir -p "$STAGE/sqlite-$v"
    echo "== faultlab: building the SQLite $v workload"
    tar -xzf "$tgz" -C "$STAGE/sqlite-$v" --strip-components=1
    if [ ! -f "$STAGE/sqlite-$v/sqlite3.c" ]; then
        echo "FAIL: sqlite3.c missing from the $v amalgamation tarball" >&2
        exit 1
    fi
    cc -O2 -static -I"$STAGE/sqlite-$v" -DSQLITE_THREADSAFE=1 -DSQLITE_OMIT_LOAD_EXTENSION \
        -o "$STAGE/sqlite-$v/faultlab-sqlite" faultlab-sqlite.c "$STAGE/sqlite-$v/sqlite3.c" -lpthread -lm
done

# The Antithesis arm is the amalgamation with the reach markers from the
# antithesishq/sqlite branch 3.51.2-instrumented, built the way their
# Dockerfile builds it (-O1, SQLITE_DEBUG, coverage callbacks at every edge)
# except static and with our edge runtime in place of their SDK. The fix in
# 3.51.3 rewrote the checkpoint write loop that three of their markers sit
# in, so that hunk is skipped on the control and it carries nine of twelve.
for v in "$SQLITE_VERSION" "$SQLITE_CONTROL_VERSION"; do
    ANT=$STAGE/ant-sqlite-$v
    rm -rf "$ANT"
    mkdir -p "$ANT"
    echo "== faultlab: building the Antithesis SQLite $v workload"
    cp "$STAGE/sqlite-$v/sqlite3.c" "$STAGE/sqlite-$v/sqlite3.h" "$ANT/"
    if [ "$v" = "$SQLITE_VERSION" ]; then
        patch -s "$ANT/sqlite3.c" <faultlab-sqlite-ant-markers.diff
    else
        patch -s -F3 -r /dev/null "$ANT/sqlite3.c" <faultlab-sqlite-ant-markers.diff || true
    fi
    patch -s "$ANT/sqlite3.c" <faultlab-sqlite-ant-header-marker.diff
    sed -i 's/"antithesis_fallback.h"/"faultlab-edge.h"/' "$ANT/sqlite3.c"
    cc -c -O1 -g -fsanitize-coverage=trace-pc -I. -I"$ANT" -DSQLITE_THREADSAFE=1 -DSQLITE_DEBUG \
        -DSQLITE_ENABLE_ANTITHESIS -DSQLITE_OMIT_LOAD_EXTENSION -o "$ANT/sqlite3.o" "$ANT/sqlite3.c"
    cc -c -O1 -g -fsanitize-coverage=trace-pc -I. -I"$ANT" -o "$ANT/workload.o" faultlab-sqlite-ant.c
    cc -c -O2 -o "$ANT/edge.o" faultlab-edge.c
    cc -static -o "$ANT/faultlab-sqlite-ant" "$ANT/sqlite3.o" "$ANT/workload.o" "$ANT/edge.o" -lpthread -lm
done

# PostgreSQL is built from source rather than taken from a distro package
# because the oracle needs contrib amcheck + pg_amcheck, which the server
# package does not ship, and because 14.3 has no current distro build at all.
build_pg() {
    version=$1 tarball=$2
    src=$BUILD_ROOT/postgresql-$version
    prefix=$STAGE/pg/$version
    echo "== faultlab: building PostgreSQL $version from source"
    rm -rf "$src" "$prefix"
    tar -xf "$tarball" -C "$BUILD_ROOT"
    (
        cd "$src"
        ./configure --prefix="$prefix" --without-readline --without-zlib \
            --disable-nls --without-icu >"$BUILD_ROOT/pg-$version-configure.log" 2>&1
        {
            make -j"$(nproc)"
            make install
            # amcheck is a contrib extension and is not built by the top-level
            # target; the pg_amcheck client that drives it ships in src/bin and
            # is already installed above.
            make -C contrib/amcheck
            make -C contrib/amcheck install
        } >"$BUILD_ROOT/pg-$version-make.log" 2>&1
    ) || { tail -n 40 "$BUILD_ROOT/pg-$version-configure.log" "$BUILD_ROOT/pg-$version-make.log"; exit 1; }
    [ -x "$prefix/bin/postgres" ] || { echo "FAIL: postgres $version not built" >&2; exit 1; }
    [ -x "$prefix/bin/pg_amcheck" ] || { echo "FAIL: pg_amcheck $version not built" >&2; exit 1; }
}
build_pg "$PG_CIC_VERSION" "$PG_CIC_TAR"
build_pg "$PG_CIC_CONTROL_VERSION" "$PG_CTL_TAR"

# --- 4. assemble the guest rootfs --------------------------------------------
echo "== faultlab: assembling rootfs"
rm -rf "$ROOT"
mkdir -p "$ROOT"/{bin,lib,lib64,etc,proc,sys,dev,tmp,run,pgmnt,bundle,w,opt}
mkdir -p "$ROOT/lib/x86_64-linux-gnu" "$ROOT/usr/lib/x86_64-linux-gnu" "$ROOT/usr/lib/postgresql"
install_libvoidstar "$ROOT"

cp "$BBOBJ/busybox" "$ROOT/bin/busybox"
for a in sh mount umount mkdir chown chmod sleep printf seq setuidgid cat echo ls \
         head tail tee env losetup poweroff reboot ln rm cp true false test expr sync id \
         grep sort uniq comm wc tr cut sed awk kill ps ip ifconfig date dd mkfifo touch; do
    ln -sf busybox "$ROOT/bin/$a"
done

[ -x "$AGENT_BIN" ] && install -m 0755 "$AGENT_BIN" "$ROOT/fault-agent"
for v in "$ETCD_VERSION" "$ETCD_CONTROL_VERSION"; do
    mkdir -p "$ROOT/opt/etcd-$v"
    install -m 0755 "$STAGE/etcd-$v/etcd" "$STAGE/etcd-$v/etcdctl" "$ROOT/opt/etcd-$v/"
done
for v in "$SQLITE_VERSION" "$SQLITE_CONTROL_VERSION"; do
    mkdir -p "$ROOT/opt/sqlite-$v"
    install -m 0755 "$STAGE/sqlite-$v/faultlab-sqlite" "$ROOT/opt/sqlite-$v/"
done
for v in "$SQLITE_VERSION" "$SQLITE_CONTROL_VERSION"; do
    mkdir -p "$ROOT/opt/ant-sqlite-$v"
    install -m 0755 "$STAGE/ant-sqlite-$v/faultlab-sqlite-ant" "$ROOT/opt/ant-sqlite-$v/"
done

# Every byte in the initramfs is unpacked into guest RAM on every single boot,
# so the image ships only the programs the bundles actually run.
PG_KEEP="postgres psql pg_isready pg_amcheck pg_ctl initdb"
for v in "$PG_CIC_VERSION" "$PG_CIC_CONTROL_VERSION"; do
    cp -a "$STAGE/pg/$v" "$ROOT/usr/lib/postgresql/$v"
    tree=$ROOT/usr/lib/postgresql/$v
    rm -rf "$tree/lib/bitcode"   # jit=off -> no LLVM bitcode
    rm -rf "$tree/include"       # headers are a build-time artifact
    for b in "$tree/bin/"*; do
        case " $PG_KEEP " in
            *" $(basename "$b") "*) ;;
            *) rm -f "$b" ;;
        esac
    done
done
find "$ROOT/usr/lib/postgresql" "$ROOT"/opt/etcd-* "$ROOT"/opt/sqlite-* "$ROOT"/opt/ant-sqlite-* -type f -perm -u+x \
    -exec strip --strip-unneeded {} + 2>/dev/null || true

# Dynamic loader + the shared-library closure of everything the guest runs, plus
# libnss_files (glibc dlopen's it for the /etc/passwd lookup postgres does).
cp -L /lib64/ld-linux-x86-64.so.2 "$ROOT/lib64/"
PGP=$ROOT/usr/lib/postgresql/$PG_CIC_VERSION
PGC=$ROOT/usr/lib/postgresql/$PG_CIC_CONTROL_VERSION
{
    for b in postgres psql pg_isready pg_amcheck pg_ctl initdb; do
        LD_LIBRARY_PATH=$PGP/lib ldd "$PGP/bin/$b" 2>/dev/null || true
        LD_LIBRARY_PATH=$PGC/lib ldd "$PGC/bin/$b" 2>/dev/null || true
    done
} | awk '/=> \// {print $3}' | sort -u >"$BUILD_ROOT/faultlab-libs.txt"
echo /lib/x86_64-linux-gnu/libnss_files.so.2 >>"$BUILD_ROOT/faultlab-libs.txt"
while read -r so; do
    [ -e "$so" ] && cp -L "$so" "$ROOT/lib/x86_64-linux-gnu/$(basename "$so")"
done <"$BUILD_ROOT/faultlab-libs.txt"

printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:%s:%s:postgres:/pgmnt:/bin/sh\n' "$PG_UID" "$PG_UID" >"$ROOT/etc/passwd"
printf 'root:x:0:\npostgres:x:%s:\n' "$PG_UID" >"$ROOT/etc/group"
printf 'passwd: files\ngroup: files\n' >"$ROOT/etc/nsswitch.conf"
printf '127.0.0.1 localhost\n' >"$ROOT/etc/hosts"
ldconfig -r "$ROOT" 2>/dev/null || true   # ld.so.cache for deterministic lib resolution

# --- 5. guest-side scripts and bundles ---------------------------------------
install -m 0644 faultlab-common.sh "$ROOT/faultlab-common.sh"
install -m 0755 faultlab-etcd-node.sh "$ROOT/w/etcd-node.sh"
install -m 0755 faultlab-etcd-ready.sh "$ROOT/w/etcd-ready.sh"
install -m 0755 faultlab-etcd-hooks.sh "$ROOT/w/etcd-hooks.sh"
install -m 0755 faultlab-sqlite-node.sh "$ROOT/w/sqlite-node.sh"
install -m 0755 faultlab-sqlite-writer.sh "$ROOT/w/sqlite-writer.sh"
install -m 0755 faultlab-sqlite-ready.sh "$ROOT/w/sqlite-ready.sh"
install -m 0755 faultlab-sqlite-hooks.sh "$ROOT/w/sqlite-hooks.sh"
install -m 0755 faultlab-sqlite-ant-node.sh "$ROOT/w/ant-sqlite-node.sh"
install -m 0755 faultlab-sqlite-ant-ready.sh "$ROOT/w/ant-sqlite-ready.sh"
install -m 0755 faultlab-sqlite-ant-hooks.sh "$ROOT/w/ant-sqlite-hooks.sh"
install -m 0755 faultlab-pg-node.sh "$ROOT/w/pg-node.sh"
install -m 0755 faultlab-pg-ready.sh "$ROOT/w/pg-ready.sh"
install -m 0755 faultlab-pg-hooks.sh "$ROOT/w/pg-hooks.sh"

# The two inits of a specimen differ only in the tree they select, so they are
# the same script with the version bound in front of it.
bind_init() {
    name=$1 var=$2 version=$3 script=$4
    { printf '#!/bin/sh\n%s=%s\nexport %s\n' "$var" "$version" "$var"
      tail -n +2 "$script"
    } >"$ROOT/$name"
    chmod 0755 "$ROOT/$name"
}
bind_init etcd-init FAULTLAB_ETCDVER "$ETCD_VERSION" faultlab-etcd-init.sh
bind_init etcd-control-init FAULTLAB_ETCDVER "$ETCD_CONTROL_VERSION" faultlab-etcd-init.sh
bind_init pgcic-init FAULTLAB_PGVER "$PG_CIC_VERSION" faultlab-pgcic-init.sh
bind_init pgcic-control-init FAULTLAB_PGVER "$PG_CIC_CONTROL_VERSION" faultlab-pgcic-init.sh
bind_init sqlite-init FAULTLAB_SQLITEVER "$SQLITE_VERSION" faultlab-sqlite-init.sh
bind_init sqlite-control-init FAULTLAB_SQLITEVER "$SQLITE_CONTROL_VERSION" faultlab-sqlite-init.sh
bind_init ant-sqlite-init FAULTLAB_SQLITEVER "$SQLITE_VERSION" faultlab-sqlite-ant-init.sh
bind_init ant-sqlite-control-init FAULTLAB_SQLITEVER "$SQLITE_CONTROL_VERSION" faultlab-sqlite-ant-init.sh

# The bundles are checked-in files rather than heredocs so the guest fault
# agent's parser and the searcher's parser are both unit-tested against the
# exact bytes the image ships.
for v in "$ETCD_VERSION" "$ETCD_CONTROL_VERSION"; do
    install -m 0644 faultlab-etcd.bundle "$ROOT/bundle/etcd-$v"
done
for v in "$PG_CIC_VERSION" "$PG_CIC_CONTROL_VERSION"; do
    install -m 0644 faultlab-pgcic.bundle "$ROOT/bundle/pgcic-$v"
done
for v in "$SQLITE_VERSION" "$SQLITE_CONTROL_VERSION"; do
    install -m 0644 faultlab-sqlite.bundle "$ROOT/bundle/sqlite-$v"
done
install -m 0644 faultlab-sqlite-ant.bundle "$ROOT/bundle/ant-sqlite"

# --- 6. bake each cluster: initdb + seed at build time ------------------------
# initdb runs once here, not in the guest: the cluster system identifier it
# mints from time/pid/random is snapshotted into the image, so there is no
# initdb-time nondeterminism left at run time.
bake_cluster() {
    version=$1
    prefix=$STAGE/pg/$version
    stagefs=$BUILD_ROOT/faultlab-stagefs-$version
    echo "== faultlab: initdb + seed for PostgreSQL $version"
    rm -rf "$stagefs"
    mkdir -p "$stagefs/pgdata"
    chown -R "$BUILD_UID:$BUILD_UID" "$stagefs"
    ( cd /tmp && setpriv --reuid="$BUILD_UID" --regid="$BUILD_UID" --clear-groups \
        env LC_ALL=C TZ=UTC "$prefix/bin/initdb" -D "$stagefs/pgdata" \
        --locale=C --encoding=UTF8 -A trust -U postgres -N ) \
        >"$BUILD_ROOT/initdb-$version.log" 2>&1 \
        || { cat "$BUILD_ROOT/initdb-$version.log"; exit 1; }
    cat >>"$stagefs/pgdata/postgresql.conf" <<EOF

# --- fault-library determinism overlay ---
listen_addresses = ''            # unix socket only — no networking nondeterminism
unix_socket_directories = '/tmp'
fsync = on
jit = off                        # no LLVM bitcode / runtime codegen variability
log_timezone = 'UTC'
timezone = 'UTC'
log_line_prefix = ''
log_statement = 'none'
shared_buffers = 32MB
max_connections = 16
autovacuum = off                 # hook 4 is the only VACUUM, so pruning is scheduled
max_wal_size = 48MB
max_parallel_workers_per_gather = 0
max_parallel_maintenance_workers = 0
EOF
    # Seed through a throwaway build-time postmaster so the image ships a
    # populated table rather than building one on every boot. Everything the
    # unprivileged build user touches — socket dir, log, cwd — lives in one
    # directory it owns; postgres refuses to run as root, and pg_ctl inherits
    # the caller's cwd, which the build user cannot read.
    work=$BUILD_ROOT/faultlab-work-$version
    rm -rf "$work"; mkdir -p "$work/sock"; chown -R "$BUILD_UID:$BUILD_UID" "$work"
    as_build() {
        setpriv --reuid="$BUILD_UID" --regid="$BUILD_UID" --clear-groups \
            env LC_ALL=C TZ=UTC HOME="$work" PWD="$work" "$@"
    }
    ( cd "$work" && as_build "$prefix/bin/pg_ctl" -D "$stagefs/pgdata" -w -t 120 \
        -o "-k $work/sock -c listen_addresses=''" -l "$work/seed.log" start ) \
        || { cat "$work/seed.log" 2>/dev/null; exit 1; }
    ( cd "$work" && as_build "$prefix/bin/psql" -h "$work/sock" -U postgres -d postgres \
        -v ON_ERROR_STOP=1 -qtAX ) <<EOF
CREATE DATABASE faultlab;
\c faultlab
CREATE EXTENSION amcheck;
CREATE TABLE cic(id int PRIMARY KEY, k int, pad text) WITH (fillfactor = $FILLFACTOR);
INSERT INTO cic SELECT g, g % 1000, repeat('x', 40) FROM generate_series(1, $SEED_ROWS) g;
CREATE INDEX cic_k_idx ON cic(k);
-- The churn hook's loop, run inside the server so each slice commits without a
-- client round trip; see faultlab-pg-hooks.sh for what the churn is for.
CREATE PROCEDURE churn(row_count int, slice_count int, round_count int) LANGUAGE plpgsql AS \$\$
DECLARE
    stride int := greatest((SELECT max(id) FROM cic) / row_count, 1);
BEGIN
    FOR r IN 1..round_count LOOP
        FOR i IN 0..slice_count - 1 LOOP
            UPDATE cic SET pad = md5(pad)
                WHERE id = ANY (ARRAY(SELECT n * stride + stride
                                      FROM generate_series(i, row_count - 1, slice_count) n));
            COMMIT;
        END LOOP;
    END LOOP;
END
\$\$;
VACUUM ANALYZE cic;
CHECKPOINT;
EOF
    ( cd "$work" && as_build "$prefix/bin/pg_ctl" -D "$stagefs/pgdata" -w -t 120 -m fast stop ) >/dev/null

    echo "== faultlab: baking fixed-UUID ext4 for PostgreSQL $version"
    chown -R "$PG_UID:$PG_UID" "$stagefs"
    ext4=$ROOT/pgdata-$version.ext4
    rm -f "$ext4"
    # lazy_*_init=0 so no background ext4 inode/journal initialization thread
    # fires at run time.
    mke2fs -q -t ext4 -U "$(ext4_uuid "$version")" \
        -E lazy_itable_init=0,lazy_journal_init=0 \
        -d "$stagefs" -F "$ext4" "$EXT4_SIZE"
}
bake_cluster "$PG_CIC_VERSION"
bake_cluster "$PG_CIC_CONTROL_VERSION"

# --- 7. pack one initramfs per specimen ---------------------------------------
# The kernel unpacks the whole cpio into guest RAM before init runs, and that
# unpack is paid on every boot. A combined image would make an etcd run carry
# two PostgreSQL clusters it never opens, so each specimen version is packed
# with only its own payload. `rdinit=` still selects the init, exactly as the
# plan specifies; there are simply six artifacts rather than one.
#
# DEVTMPFS_MOUNT gives the guest /dev before init runs, so no device nodes are
# baked. Sorted entries, fixed mtimes, owner 0:0, gzip -n.
find "$ROOT" -mindepth 1 -exec touch -hcd @0 {} +

pack_image() {
    name=$1
    shift
    stage=$BUILD_ROOT/faultlab-pack
    rm -rf "$stage"
    cp -al "$ROOT" "$stage"
    for drop in "$@"; do
        # shellcheck disable=SC2086  # drop patterns are deliberate globs
        rm -rf "${stage:?}"/$drop
    done
    ( cd "$stage" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
        | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/$name"
    echo "ok: $ART_DIR/$name ($(du -h "$ART_DIR/$name" | cut -f1) packed, \
$(du -sh --apparent-size "$stage" | cut -f1) unpacked)"
    echo "    sha256 $(sha256_of "$ART_DIR/$name")"
    rm -rf "$stage"
}

echo "== faultlab: packing initramfs images"
V=$PG_CIC_VERSION
C=$PG_CIC_CONTROL_VERSION
E=$ETCD_VERSION
F=$ETCD_CONTROL_VERSION
S=$SQLITE_VERSION
T=$SQLITE_CONTROL_VERSION
PG_ALL=("usr/lib/postgresql" "pgdata-*.ext4" "pgcic-init" "pgcic-control-init" "w/pg-*" "bundle/pgcic-*")
ETCD_ALL=("opt/etcd-*" "etcd-init" "etcd-control-init" "w/etcd-*" "bundle/etcd-*")
SQLITE_ALL=("opt/sqlite-*" "sqlite-init" "sqlite-control-init" "w/sqlite-*" "bundle/sqlite-*")
ANT_ALL=("opt/ant-sqlite-*" "ant-sqlite-init" "ant-sqlite-control-init" "w/ant-sqlite-*" "bundle/ant-sqlite")
pack_image "${ARTIFACT%.cpio.gz}-etcd-$E.cpio.gz" \
    "${PG_ALL[@]}" "${SQLITE_ALL[@]}" "${ANT_ALL[@]}" "opt/etcd-$F" "etcd-control-init" "bundle/etcd-$F"
pack_image "${ARTIFACT%.cpio.gz}-etcd-$F.cpio.gz" \
    "${PG_ALL[@]}" "${SQLITE_ALL[@]}" "${ANT_ALL[@]}" "opt/etcd-$E" "etcd-init" "bundle/etcd-$E"
pack_image "${ARTIFACT%.cpio.gz}-pgcic-$V.cpio.gz" \
    "${ETCD_ALL[@]}" "${SQLITE_ALL[@]}" "${ANT_ALL[@]}" "usr/lib/postgresql/$C" "pgdata-$C.ext4" "pgcic-control-init" "bundle/pgcic-$C"
pack_image "${ARTIFACT%.cpio.gz}-pgcic-$C.cpio.gz" \
    "${ETCD_ALL[@]}" "${SQLITE_ALL[@]}" "${ANT_ALL[@]}" "usr/lib/postgresql/$V" "pgdata-$V.ext4" "pgcic-init" "bundle/pgcic-$V"
pack_image "${ARTIFACT%.cpio.gz}-sqlite-$S.cpio.gz" \
    "${PG_ALL[@]}" "${ETCD_ALL[@]}" "${ANT_ALL[@]}" "opt/sqlite-$T" "sqlite-control-init" "bundle/sqlite-$T"
pack_image "${ARTIFACT%.cpio.gz}-sqlite-$T.cpio.gz" \
    "${PG_ALL[@]}" "${ETCD_ALL[@]}" "${ANT_ALL[@]}" "opt/sqlite-$S" "sqlite-init" "bundle/sqlite-$S"
pack_image "${ARTIFACT%.cpio.gz}-ant-sqlite-$S.cpio.gz" \
    "${PG_ALL[@]}" "${ETCD_ALL[@]}" "${SQLITE_ALL[@]}" "opt/ant-sqlite-$T" "ant-sqlite-control-init"
pack_image "${ARTIFACT%.cpio.gz}-ant-sqlite-$T.cpio.gz" \
    "${PG_ALL[@]}" "${ETCD_ALL[@]}" "${SQLITE_ALL[@]}" "opt/ant-sqlite-$S" "ant-sqlite-init"
