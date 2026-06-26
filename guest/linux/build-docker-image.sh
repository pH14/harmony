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
require_tools cc make gzip bzip2 cpio gunzip jq tar
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
[ -x "$BUNDLE/rootfs/usr/local/bin/docker-entrypoint.sh" ] \
    || { echo "FAIL: docker-entrypoint.sh not in the extracted rootfs" >&2; exit 1; }

# The SAME fixed insert/select workload as task 37, baked INTO the container
# rootfs so `runc exec ... psql -f /workload.sql` drives the live DB over its
# local unix socket: CREATE then N autocommit INSERT+SELECT iterations, each
# reporting the row plus a running count/sum. Values are a pure function of the
# loop index (no wall-clock / random columns) → the golden is a deterministic
# function of the seed (identical rows to task 37).
{
    echo "CREATE TABLE ledger(i int primary key, v bigint);"
    i=1
    while [ "$i" -le "$WORKLOAD_N" ]; do
        echo "INSERT INTO ledger(i,v) VALUES ($i, $i::bigint*$i + 7);"
        echo "SELECT 'row', i, v, (SELECT count(*) FROM ledger), (SELECT sum(v) FROM ledger) FROM ledger WHERE i=$i;"
        i=$((i+1))
    done
} >"$BUNDLE/rootfs/workload.sql"

echo "== docker image: generating the OCI config.json"
RUNC=$DK_STAGE/docker/runc                      # the real runc (not the wrapper)
"$RUNC" spec --bundle "$BUNDLE"                 # default template -> $BUNDLE/config.json
IMG_ENV=$(jq -c '.config.Env // []' "$CFG")
IMG_ARGS=$(jq -cn \
    --argjson e "$(jq -c '.config.Entrypoint // []' "$CFG")" \
    --argjson c "$(jq -c '.config.Cmd // []' "$CFG")" '$e + $c')
IMG_CWD=$(jq -r '.config.WorkingDir // "/"' "$CFG")
# Docker's default capability set. The bare `runc spec` template grants only 3
# caps (AUDIT_WRITE/KILL/NET_BIND_SERVICE) — too few for the postgres entrypoint,
# which chowns PGDATA and `gosu`/`setpriv`s to the postgres user (needs CHOWN,
# DAC_OVERRIDE, FOWNER, SETUID, SETGID, …). Grant the same set docker would, so
# the off-the-shelf image's entrypoint runs unchanged.
CAPS='["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
       "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
       "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"]'
# Patch the template with the image's runtime config. terminal=false so the
# container's stdout/stderr are inherited from `runc run` (= ttyS0). The default
# template's namespaces include a fresh NETWORK namespace with no veth — i.e.
# `--network none` (loopback only); the workload reaches postgres over the local
# unix socket. POSTGRES_HOST_AUTH_METHOD=trust runs the off-the-shelf image
# without a password.
# Allow all devices (the bare `runc spec` template's default-deny device cgroup —
# `[{allow:false}]` with no allow rules — kills the container's PID 1 on the
# guest kernel: cgroup-v2 device control is an eBPF filter, and with everything
# denied the init dies at exec). This is a trusted single-purpose determinism
# gate (not a hostile multi-tenant host), so allow-all is appropriate.
# noNewPrivileges=false matches docker's default (the entrypoint `gosu`s).
jq --argjson env "$IMG_ENV" --argjson args "$IMG_ARGS" --arg cwd "$IMG_CWD" \
   --argjson caps "$CAPS" '
    .process.terminal = false
  | .process.args = $args
  | .process.cwd = (if $cwd == "" then "/" else $cwd end)
  | .process.env = ($env + ["POSTGRES_HOST_AUTH_METHOD=trust", "TERM=xterm"])
  | .process.capabilities.bounding = $caps
  | .process.capabilities.effective = $caps
  | .process.capabilities.permitted = $caps
  | .process.noNewPrivileges = false
  | .root.path = "rootfs"
  | .root.readonly = false
  | .linux.cgroupsPath = "pg-container"
  | .linux.resources.devices = [{"allow": true, "access": "rwm"}]
' "$BUNDLE/config.json" >"$BUNDLE/config.json.new"
mv "$BUNDLE/config.json.new" "$BUNDLE/config.json"
echo "   args=$IMG_ARGS cwd=${IMG_CWD:-/}  rootfs=$(du -sh "$BUNDLE/rootfs" | cut -f1)"

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
