#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **fault-library workload initramfs** -> initramfs-faultlab.cpio.gz.
#
# One image carries both historical specimens, selected by `rdinit=`:
#   /etcd-init          etcd 3.5.2, the consistent-index inconsistency
#                       (bugs/historical/etcd-3.5-inconsistency)
#   /pgcic-init         PostgreSQL 14.3, the CREATE INDEX CONCURRENTLY race
#                       (bugs/historical/postgres-cic-corruption)
#   /pgcic-control-init PostgreSQL 14.4, that bug's fixed control arm
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
require_tools cc make gzip bzip2 cpio mke2fs setpriv ldd ldconfig losetup curl

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
FILLFACTOR=70                                 # leave page room so UPDATEs stay HOT

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
        echo "== faultlab: fetching $(basename "$url")"
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
mkdir -p "$STAGE/etcd"
echo "== faultlab: extracting etcd $ETCD_VERSION"
tar -xzf "$ETCD_TGZ" -C "$STAGE/etcd" --strip-components=1
if [ ! -x "$STAGE/etcd/etcd" ] || [ ! -x "$STAGE/etcd/etcdctl" ]; then
    echo "FAIL: etcd/etcdctl missing from the release tarball" >&2
    exit 1
fi

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
mkdir -p "$ROOT/opt/etcd"
install -m 0755 "$STAGE/etcd/etcd" "$STAGE/etcd/etcdctl" "$ROOT/opt/etcd/"

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
find "$ROOT/usr/lib/postgresql" "$ROOT/opt/etcd" -type f -perm -u+x \
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
install -m 0755 faultlab-etcd-init.sh "$ROOT/etcd-init"
install -m 0755 faultlab-etcd-node.sh "$ROOT/w/etcd-node.sh"
install -m 0755 faultlab-etcd-ready.sh "$ROOT/w/etcd-ready.sh"
install -m 0755 faultlab-etcd-hooks.sh "$ROOT/w/etcd-hooks.sh"
install -m 0755 faultlab-pg-node.sh "$ROOT/w/pg-node.sh"
install -m 0755 faultlab-pg-ready.sh "$ROOT/w/pg-ready.sh"
install -m 0755 faultlab-pg-hooks.sh "$ROOT/w/pg-hooks.sh"

# The two Postgres inits differ only in the tree they select, so they are the
# same script with the version bound in front of it.
for pair in "pgcic-init:$PG_CIC_VERSION" "pgcic-control-init:$PG_CIC_CONTROL_VERSION"; do
    name=${pair%%:*} version=${pair##*:}
    { printf '#!/bin/sh\nFAULTLAB_PGVER=%s\nexport FAULTLAB_PGVER\n' "$version"
      tail -n +2 faultlab-pgcic-init.sh
    } >"$ROOT/$name"
    chmod 0755 "$ROOT/$name"
done

# The bundles are checked-in files rather than heredocs so the guest fault
# agent's parser and the searcher's parser are both unit-tested against the
# exact bytes the image ships.
install -m 0644 faultlab-etcd.bundle "$ROOT/bundle/etcd"
for v in "$PG_CIC_VERSION" "$PG_CIC_CONTROL_VERSION"; do
    install -m 0644 faultlab-pgcic.bundle "$ROOT/bundle/pgcic-$v"
done

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
# two PostgreSQL clusters it never opens, so each specimen is packed with only
# its own payload. `rdinit=` still selects the init, exactly as the plan
# specifies; there are simply three artifacts rather than one.
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
pack_image "${ARTIFACT%.cpio.gz}-etcd.cpio.gz" \
    "usr/lib/postgresql" "pgdata-*.ext4" "pgcic-init" "pgcic-control-init" "w/pg-*"
pack_image "${ARTIFACT%.cpio.gz}-pgcic-$V.cpio.gz" \
    "opt/etcd" "etcd-init" "w/etcd-*" "bundle/etcd" \
    "usr/lib/postgresql/$C" "pgdata-$C.ext4" "pgcic-control-init" "bundle/pgcic-$C"
pack_image "${ARTIFACT%.cpio.gz}-pgcic-$C.cpio.gz" \
    "opt/etcd" "etcd-init" "w/etcd-*" "bundle/etcd" \
    "usr/lib/postgresql/$V" "pgdata-$V.ext4" "pgcic-init" "bundle/pgcic-$V"
