#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build M3's real PostgreSQL container payload natively on Linux/aarch64.
# PostgreSQL, its clients, BusyBox, and musl are all static and LSE-only; every
# shipped ELF is rejected if it contains LL/SC or a live generic-counter access.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make gzip cpio python3 readelf patch perl flex bison chroot
if [ "$(id -u)" -ne 0 ]; then
    echo "FAIL: build-arm64-postgres-image.sh needs root for build-time chroot/initdb" >&2
    exit 1
fi

pg_pristine=$BUILD_ROOT/postgresql-$PG_SOURCE_VERSION
pg_source=$BUILD_ROOT/arm64-postgres-pg-src
pg_object=$BUILD_ROOT/arm64-postgres-pg-build
postgres_root=$BUILD_ROOT/arm64-postgres-root
container_root=$postgres_root/container
busybox_object=$BUILD_ROOT/busybox-build-arm64-postgres
guest_prefix=/opt/harmony/postgres
stage_prefix=$container_root$guest_prefix
pgdata=/var/lib/postgresql/data
workload_n=20

extract_busybox
prepare_busybox_build_source
verify_and_extract \
    "$DL_DIR/$(basename "$PG_SOURCE_URL")" "$PG_SOURCE_SHA256" "$pg_pristine"

echo "== arm64 postgres image: pristine PostgreSQL $PG_SOURCE_VERSION source"
rm -rf "$pg_source" "$pg_object"
cp -a "$pg_pristine" "$pg_source"
patch -d "$pg_source" --batch -p1 \
    <"$LINUX_DIR/patches/postgresql/0001-static-bootstrap-without-plpgsql.patch"
[ "$(grep -c '/bin/pwd' "$pg_source/configure")" -eq 2 ] || {
    echo "FAIL: PostgreSQL configure /bin/pwd anchors changed" >&2
    exit 1
}
sed 's#/bin/pwd#pwd#g' "$pg_source/configure" >"$pg_source/configure.tmp"
mv "$pg_source/configure.tmp" "$pg_source/configure"
chmod +x "$pg_source/configure"

echo "== arm64 postgres image: building LSE-only static musl ($MUSL_VERSION)"
build_arm64_game_musl
musl_cc=$ARM64_GAME_MUSL_PREFIX/bin/musl-gcc

echo "== arm64 postgres image: configuring static PostgreSQL"
mkdir -p "$pg_object"
path_map_flags=
if [ -n "${HARMONY_BUILD_PATH_PREFIX:-}" ]; then
    path_map_flags="-ffile-prefix-map=$HARMONY_BUILD_PATH_PREFIX=/build -fdebug-prefix-map=$HARMONY_BUILD_PATH_PREFIX=/build -fmacro-prefix-map=$HARMONY_BUILD_PATH_PREFIX=/build"
fi
(
    cd "$pg_object"
    CC="$musl_cc" \
    CFLAGS="-O2 -march=armv8.1-a+lse -mno-outline-atomics $path_map_flags" \
    LDFLAGS='-static' \
        "$pg_source/configure" \
        --prefix="$guest_prefix" \
        --disable-rpath --disable-nls --without-icu --without-readline --without-zlib
)

# Static archive order matters for the frontend programs. Repeating the owned
# common/port archives closes that order without adding a dynamic dependency.
frontend_libs="-L$pg_object/src/common -lpgcommon_shlib -lpgcommon -L$pg_object/src/port -lpgport -lm"
echo "== arm64 postgres image: building server, client, controller, and build-time initdb"
make -C "$pg_object/src/backend" generated-headers
make -C "$pg_object/src/port" -j"$(nproc)" all
make -C "$pg_object/src/common" -j"$(nproc)" all
make -C "$pg_object/src/backend" -j"$(nproc)" postgres
make -C "$pg_object/src/bin/psql" -j"$(nproc)" psql LIBS="$frontend_libs"
make -C "$pg_object/src/bin/pg_ctl" -j"$(nproc)" pg_ctl LIBS="$frontend_libs"
# Only the build-time initdb needs this exception: the static frontend archive
# closure repeats identical upstream encoding-name objects. initdb is removed
# before publication and every shipped binary remains under the normal link.
make -C "$pg_object/src/bin/initdb" -j"$(nproc)" initdb \
    LDFLAGS_EX='-Wl,--allow-multiple-definition' LIBS="$frontend_libs"

echo "== arm64 postgres image: building explicit static BusyBox surface"
rm -rf "$busybox_object"
mkdir -p "$busybox_object" "$ARM64_ART_DIR"
make -C "$BBSRC" O="$busybox_object" allnoconfig >/dev/null

enable_busybox_symbol() {
    local symbol=$1
    if grep -qxF "CONFIG_${symbol}=y" "$busybox_object/.config"; then
        return
    fi
    grep -qxF "# CONFIG_${symbol} is not set" "$busybox_object/.config" || {
        echo "FAIL: BusyBox has no disabled CONFIG_${symbol} setting" >&2
        exit 1
    }
    sed "s/^# CONFIG_${symbol} is not set$/CONFIG_${symbol}=y/" \
        "$busybox_object/.config" >"$busybox_object/.config.tmp"
    mv "$busybox_object/.config.tmp" "$busybox_object/.config"
}

for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKNOD CHMOD CHOWN CHROOT \
    MKDIR CAT ECHO GREP HALT REBOOT SETUIDGID SYNC TEE UNSHARE DMESG; do
    enable_busybox_symbol "$symbol"
done
sed 's/^CONFIG_EXTRA_CFLAGS=""$/CONFIG_EXTRA_CFLAGS="-march=armv8.1-a+lse -mno-outline-atomics"/' \
    "$busybox_object/.config" >"$busybox_object/.config.tmp"
mv "$busybox_object/.config.tmp" "$busybox_object/.config"
set +o pipefail
yes '' | make -C "$BBSRC" O="$busybox_object" oldconfig >/dev/null
set -o pipefail
make -C "$BBSRC" O="$busybox_object" CC="$musl_cc" -j"$(nproc)" busybox >/dev/null
for symbol in STATIC BUSYBOX ASH SH_IS_ASH MOUNT MKNOD CHMOD CHOWN CHROOT \
    MKDIR CAT ECHO GREP HALT REBOOT SETUIDGID SYNC TEE UNSHARE DMESG; do
    grep -qxF "CONFIG_${symbol}=y" "$busybox_object/.config" || {
        echo "FAIL: arm64 postgres BusyBox lost CONFIG_${symbol}" >&2
        exit 1
    }
done

echo "== arm64 postgres image: assembling outer and container roots"
rm -rf "$postgres_root"
mkdir -p "$postgres_root"/{bin,dev,proc,sys,run,tmp}
mkdir -p "$container_root"/{bin,dev/shm,etc,proc,tmp,var/lib/postgresql}
mkdir -p "$stage_prefix"/{bin,lib,share}
"$musl_cc" -static -Os -march=armv8.1-a+lse -mno-outline-atomics \
    -Wall -Wextra -Werror \
    "$LINUX_DIR/arm64-mmio-console.c" -o "$postgres_root/bin/mmio-console"

install -m 0755 "$busybox_object/busybox" "$postgres_root/bin/busybox"
install -m 0755 "$busybox_object/busybox" "$container_root/bin/busybox"
for applet in sh mount mknod chmod chown chroot mkdir cat echo grep halt reboot \
    setuidgid sync tee unshare dmesg; do
    ln -sf busybox "$postgres_root/bin/$applet"
done
for applet in sh setuidgid echo; do
    ln -sf busybox "$container_root/bin/$applet"
done

install -m 0755 "$pg_object/src/backend/postgres" "$stage_prefix/bin/postgres"
install -m 0755 "$pg_object/src/bin/psql/psql" "$stage_prefix/bin/psql"
install -m 0755 "$pg_object/src/bin/pg_ctl/pg_ctl" "$stage_prefix/bin/pg_ctl"
install -m 0755 "$pg_object/src/bin/initdb/initdb" "$stage_prefix/bin/initdb"

# initdb's static bootstrap data. Snowball and PL/pgSQL normally dlopen shared
# modules; the fixed SQL acceptance workload uses neither, so Snowball is an
# explicit empty command stream and the source patch omits PL/pgSQL creation.
install -m 0644 "$pg_object/src/include/catalog/postgres.bki" \
    "$stage_prefix/share/postgres.bki"
install -m 0644 "$pg_object/src/include/catalog/system_constraints.sql" \
    "$stage_prefix/share/system_constraints.sql"
for file in information_schema.sql sql_features.txt system_functions.sql system_views.sql; do
    install -m 0644 "$pg_source/src/backend/catalog/$file" \
        "$stage_prefix/share/$file"
done
install -m 0644 "$pg_source/src/backend/libpq/pg_hba.conf.sample" \
    "$stage_prefix/share/pg_hba.conf.sample"
install -m 0644 "$pg_source/src/backend/libpq/pg_ident.conf.sample" \
    "$stage_prefix/share/pg_ident.conf.sample"
install -m 0644 "$pg_source/src/backend/utils/misc/postgresql.conf.sample" \
    "$stage_prefix/share/postgresql.conf.sample"
: >"$stage_prefix/share/snowball_create.sql"
make -s -C "$pg_object/src/timezone" install DESTDIR="$container_root"

printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:65534:65534:postgres:/var/lib/postgresql:/bin/sh\n' \
    >"$container_root/etc/passwd"
printf 'root:x:0:\npostgres:x:65534:\n' >"$container_root/etc/group"
printf 'passwd: files\ngroup: files\nhosts: files\n' >"$container_root/etc/nsswitch.conf"
printf '127.0.0.1 localhost\n::1 localhost\n' >"$container_root/etc/hosts"
: >"$container_root/etc/resolv.conf"
chmod 1777 "$container_root/tmp" "$container_root/dev/shm"

# Functional device nodes for build-time initdb. Runtime replaces /dev with a
# bind mount from the guest devtmpfs inside the container's mount namespace.
mknod -m 0666 "$container_root/dev/null" c 1 3
mknod -m 0666 "$container_root/dev/urandom" c 1 9
install -d -o 65534 -g 65534 -m 0700 "$container_root$pgdata"
echo "== arm64 postgres image: pre-baking PGDATA as uid 65534"
chroot --userspec=65534:65534 "$container_root" \
    "$guest_prefix/bin/initdb" -D "$pgdata" --no-locale --encoding=UTF8 \
    --auth-local=trust --auth-host=trust -U postgres -N
cat >>"$container_root$pgdata/postgresql.conf" <<'EOF'

# M3 deterministic static-container overlay.
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
chown 65534:65534 "$container_root$pgdata/postgresql.conf"
rm "$stage_prefix/bin/initdb"

{
    echo "CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);"
    i=1
    while [ "$i" -le "$workload_n" ]; do
        echo "INSERT INTO ledger(i,t) VALUES ($i, clock_timestamp());"
        echo "SELECT 'row', i, (SELECT count(*) FROM ledger), (SELECT sum(i) FROM ledger), id, t FROM ledger WHERE i=$i;"
        i=$((i + 1))
    done
} >"$container_root/workload.sql"
install -m 0755 "$LINUX_DIR/arm64-postgres-run.sh" "$container_root/run-workload.sh"
install -m 0755 "$LINUX_DIR/arm64-postgres-container-setup.sh" \
    "$postgres_root/arm64-postgres-container-setup.sh"
install -m 0755 "$LINUX_DIR/arm64-postgres-init.sh" "$postgres_root/init"

if [ "$(stat -c %u "$container_root$pgdata")" -ne 65534 ]; then
    echo "FAIL: packed PGDATA is not owned by the runtime postgres uid" >&2
    exit 1
fi

echo "== arm64 postgres image: scanning every shipped ELF mapping"
while read -r binary; do
    if readelf -h "$binary" >/dev/null 2>&1; then
        python3 "$GUEST_DIR/scripts/aa4-exclusive-scan.py" "$binary"
        python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" "$binary"
    fi
done < <(find "$postgres_root" \( -type f -perm -0100 -o -type f -name '*.so*' \) \
    | LC_ALL=C sort)

# Publish the payloads as first-class N5 outputs as well as embedding them in
# the initramfs. This makes the lock-built binary closure directly attestable.
install -m 0755 "$stage_prefix/bin/postgres" "$ARM64_ART_DIR/postgres"
install -m 0755 "$stage_prefix/bin/psql" "$ARM64_ART_DIR/psql"
install -m 0755 "$stage_prefix/bin/pg_ctl" "$ARM64_ART_DIR/pg_ctl"

echo "== arm64 postgres image: capturing the clean canonical guest snapshot"
find "$postgres_root" -mindepth 1 -exec touch -hcd @0 {} +
(cd "$postgres_root" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --reproducible --quiet) \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs-postgres.cpio.gz"
snapshot_sha=$(sha256_of "$ARM64_ART_DIR/initramfs-postgres.cpio.gz")
printf '%s  initramfs-postgres.cpio.gz\n' "$snapshot_sha" \
    >"$ARM64_ART_DIR/initramfs-postgres.cpio.gz.sha256"
echo "ok: canonical snapshot sha256=$snapshot_sha"
