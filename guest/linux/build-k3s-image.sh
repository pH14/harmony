#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **Postgres-on-k3s workload initramfs** (task 49 — the determinism
# stress test at full stack height: a single-node lightweight Kubernetes cluster
# inside the deterministic guest, with a CLIENT pod making calls to a POSTGRES
# server pod over the in-guest CNI, deterministic-twice).
#
# **What runs (see guest/linux/IMPLEMENTATION.md + tasks/49-*).** One single-vCPU
# guest boots `k3s` (a lightweight Kubernetes distro: ONE static Go binary that
# bundles containerd + runc + the flannel/bridge/host-local CNI + kube-proxy +
# kubectl, with a sqlite datastore). `k3s-init.sh` brings the cluster up, then:
#   * a `postgres` Pod runs the official `postgres:17` OCI image on a **pre-baked
#     PGDATA** (build-time initdb, baked here as a hostPath, uid 999) listening on
#     TCP, fronted by a fixed-ClusterIP `postgres` Service;
#   * a `client` Pod (the same postgres:17 image, for its `psql`) connects to that
#     Service **over the cluster CNI** (pod -> ClusterIP -> kube-proxy DNAT -> the
#     server pod, all intra-guest — NO host networking, pv-net unused) and runs the
#     task-42 gen_random_uuid()/clock_timestamp() workload, streaming
#     `row|i|count|sum|uuid|t` to its pod log -> ttyS0.
#
# **Why k3s makes progress (the determinism is preemption-driven — task 47/54).**
# kubelet + containerd + apiserver + scheduler + controller-manager + kube-proxy +
# flannel are all Go/multi-goroutine services that busy-spin and depend on
# preemption. Under the V-time VMM the LAPIC timer **preempts** a busy-spinning
# thread at the seed-deterministic V-time deadline (run_until) and the idle-HLT
# resume warps to the next deadline (task 52), so the Go schedulers run and the
# cluster converges — deterministically, because every preemption instant is a
# pure function of the seed. k3s mints its certs/tokens/SA-keys/object-UIDs from
# getrandom -> the seeded CRNG and stamps every resource/lease/event from the
# V-time clock, so two same-seed boots are bit-identical (incl. the workload's
# "random" UUIDs + wall-clock timestamps).
#
# **No kernel change.** The task-36 Kata container-host bzImage already builds in
# the full k8s surface (BRIDGE/VETH/VXLAN/NF_CONNTRACK/NF_NAT/NF_TABLES/IP_VS/
# OVERLAY_FS/the iptables + netfilter_xt set + cgroup-v2/namespaces); the
# determinism overlay disables none of it. The companion kernel is the *unchanged*
# task-36 bzImage. The k3s data dir, sqlite, container rootfs layers and PGDATA all
# live in the initramfs tmpfs (RAM) -> deterministic VM-memory writes.
#
# Linux + root only (mounts, cgroup, chroot, the static layout assume a Linux
# build host; the box is the pinned build environment). See docs/BUILDING.md.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2 cpio gunzip jq tar chroot mount umount

if [ "$(id -u)" != "0" ]; then
    echo "FAIL: build-k3s-image.sh must run as root (the cpio preserves the uid-999" >&2
    echo "      PGDATA ownership, and the static layout mirrors the privileged guest" >&2
    echo "      rootfs)." >&2
    exit 1
fi

# --- tunables ----------------------------------------------------------------
K3SROOT=$BUILD_ROOT/k3s-root                    # the assembled guest rootfs
PG_IMAGE_TAR=$DL_DIR/postgres-image.tar         # the official postgres image (task 38)
PAUSE_IMAGE_TAR=$DL_DIR/k3s-pause-image.tar     # the pause/sandbox image (fetch.sh)
K3S_BIN=$DL_DIR/k3s                             # the pinned k3s binary
WORKLOAD_N=20                                   # fixed insert/select iterations (== task 37/42)
PG_CLUSTERIP=10.43.0.100                        # fixed Service ClusterIP (svc CIDR 10.43.0.0/16)

# --- 0. verify the pinned inputs ---------------------------------------------
echo "== k3s image: verifying pinned inputs"
verify_pin() {  # <file> <sha> <hint>
    [ -f "$1" ] || { echo "FAIL: $1 missing — $3" >&2; exit 1; }
    got=$(sha256_of "$1")
    [ "$got" = "$2" ] || { echo "FAIL: $1 sha256 mismatch (want $2, got $got)" >&2; exit 1; }
}
verify_pin "$K3S_BIN" "$K3S_BIN_SHA256" "run 'make -C guest fetch' first"
if [ ! -f "$PG_IMAGE_TAR" ] || [ ! -s "$PG_IMAGE_TAR" ]; then
    echo "FAIL: $PG_IMAGE_TAR missing/empty — run 'make -C guest fetch' on the box" >&2
    echo "      (ctr+network; integrity anchored by $POSTGRES_IMAGE_INDEX_DIGEST)." >&2
    exit 1
fi
if [ ! -f "$PAUSE_IMAGE_TAR" ] || [ ! -s "$PAUSE_IMAGE_TAR" ]; then
    echo "FAIL: $PAUSE_IMAGE_TAR missing/empty — run 'make -C guest fetch' on the box" >&2
    echo "      (ctr; extracted from the pinned k3s air-gap tarball)." >&2
    exit 1
fi

# --- 1. static busybox (mirrors build-docker-image.sh; self-contained) -------
echo "== k3s image: building static busybox ($BUSYBOX_VERSION)"
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
echo "== k3s image: assembling rootfs"
# DEFENSIVE: umount any leaked build-time bind mounts before rm -rf (rm -rf
# crosses mount points — deleting an un-umounted /dev bind would wipe host /dev).
umount "$K3SROOT/pgstage/proc" 2>/dev/null || true
umount "$K3SROOT/pgstage/dev" 2>/dev/null || true
rm -rf "$K3SROOT"
mkdir -p "$K3SROOT"/{bin,sbin,etc,proc,sys,dev,tmp,root,run}
mkdir -p "$K3SROOT/usr/local/bin" "$K3SROOT/sys/fs/cgroup" "$K3SROOT/var/lib" \
         "$K3SROOT/etc/rancher/k3s" "$K3SROOT/var/lib/rancher/k3s/agent/images" \
         "$K3SROOT/var/lib/rancher/k3s/server/manifests" "$K3SROOT/k8s"
ln -sf /run "$K3SROOT/var/run"

cp "$BBOBJ/busybox" "$K3SROOT/bin/busybox"
# /bin/sh is the /init interpreter; the rest let any `sh -c`/PATH lookup the init
# (or k3s' shell-outs to `ip`, `mount`, ...) does resolve without full coreutils.
for a in sh mount umount mkdir chmod chown cat echo grep sleep kill nice ln rm cp \
         true false test sync reboot poweroff head tail env printf cut wc ps sed \
         cmp ls id mv touch dd find xargs awk tr sort uniq date hostname dmesg \
         mountpoint nproc seq tee timeout ip; do
    ln -sf busybox "$K3SROOT/bin/$a"
done

# The k3s binary (one static Go binary). k3s dispatches its bundled tools by
# argv[0], so symlink kubectl/crictl/ctr to it (the init also uses `k3s kubectl`).
install -m 0755 "$K3S_BIN" "$K3SROOT/usr/local/bin/k3s"
for t in kubectl crictl ctr; do ln -sf k3s "$K3SROOT/usr/local/bin/$t"; done

# Minimal /etc. Static Go uses pure-Go nss (reads these files directly).
printf 'root:x:0:0:root:/root:/bin/sh\npostgres:x:999:999:postgres:/var/lib/postgresql:/bin/sh\nnobody:x:65534:65534:nobody:/:/bin/sh\n' >"$K3SROOT/etc/passwd"
printf 'root:x:0:\npostgres:x:999:\nnobody:x:65534:\n' >"$K3SROOT/etc/group"
printf 'passwd: files\ngroup: files\nhosts: files\n' >"$K3SROOT/etc/nsswitch.conf"
printf '127.0.0.1 localhost\n::1 localhost\n' >"$K3SROOT/etc/hosts"
: >"$K3SROOT/etc/resolv.conf"                   # in-guest only; CoreDNS disabled

# --- 3. pre-import images: the pause/sandbox + the official postgres image ----
# k3s auto-imports every *.tar under /var/lib/rancher/k3s/agent/images/ into its
# containerd image store at agent startup — the air-gap path, so the guest needs
# NO registry/network. We import only the pause image (the sandbox every pod
# needs; coredns/traefik/etc. are --disabled) and the official postgres:17 image
# (reused for BOTH pods — the server runs `postgres`, the client uses its `psql`).
echo "== k3s image: staging the pre-imported container images"
cp "$PAUSE_IMAGE_TAR" "$K3SROOT/var/lib/rancher/k3s/agent/images/k3s-pause.tar"
cp "$PG_IMAGE_TAR"    "$K3SROOT/var/lib/rancher/k3s/agent/images/postgres.tar"

# --- 4. pre-bake PGDATA (build-time initdb as uid 999) -----------------------
# Exactly task 37/38's pattern, and load-bearing the same way: running the
# official image's *entrypoint* would initdb at pod start (crushingly slow under
# the single-stepping VMM) AND re-exec through `gosu` (a Go program whose runtime
# busy-spins). So we initdb ONCE here into a hostPath the server pod mounts, and
# the pod runs the `postgres` binary directly (no entrypoint, no gosu, no runtime
# initdb). We extract the image rootfs to a throwaway staging tree just to get its
# `initdb`/`postgres` binaries + libs; only the resulting PGDATA is baked.
echo "== k3s image: extracting the postgres image rootfs (for build-time initdb)"
PGSTAGE=$K3SROOT/pgstage
rm -rf "$PGSTAGE"; mkdir -p "$PGSTAGE"
IMG=$BUILD_ROOT/k3s-pg-img
rm -rf "$IMG"; mkdir -p "$IMG"
tar -xf "$PG_IMAGE_TAR" -C "$IMG"
MANI=$IMG/manifest.json
[ -f "$MANI" ] || { echo "FAIL: no manifest.json in the postgres image export" >&2; exit 1; }
for layer in $(jq -r '.[0].Layers[]' "$MANI"); do
    tar -xf "$IMG/$layer" -C "$PGSTAGE"
    find "$PGSTAGE" -name '.wh..wh..opq' -delete 2>/dev/null || true
    find "$PGSTAGE" -name '.wh.*' 2>/dev/null | while read -r wh; do
        target="$(dirname "$wh")/$(basename "$wh" | sed 's/^\.wh\.//')"
        rm -rf "${target:?}" "$wh"
    done
done
PGBIN=/usr/lib/postgresql/$PG_MAJOR/bin
[ -x "$PGSTAGE$PGBIN/initdb" ] || { echo "FAIL: initdb not in the postgres image rootfs" >&2; exit 1; }

echo "== k3s image: pre-baking PGDATA (build-time initdb as uid 999)"
PGDATA_REL=/var/lib/postgresql/data
# initdb needs /dev/null + /proc; bind the host's into the staging rootfs (the
# EXIT trap + the §2 defensive umount guarantee they never leak into a later rm).
mkdir -p "$PGSTAGE/dev" "$PGSTAGE/proc" "$PGSTAGE$PGDATA_REL"
chown 999:999 "$PGSTAGE/var/lib/postgresql" "$PGSTAGE$PGDATA_REL"
mount --bind /dev "$PGSTAGE/dev"
mount -t proc proc "$PGSTAGE/proc"
trap 'umount "$PGSTAGE/proc" 2>/dev/null || true; umount "$PGSTAGE/dev" 2>/dev/null || true' EXIT
chroot --userspec=999:999 "$PGSTAGE" /bin/sh -c "
    cd /var/lib/postgresql
    export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC HOME=/var/lib/postgresql
    exec $PGBIN/initdb -D $PGDATA_REL \
        --locale=C.UTF-8 --encoding=UTF8 --auth-local=trust --auth-host=trust -U postgres -N
" >"$BUILD_ROOT/initdb-k3s.log" 2>&1 || { cat "$BUILD_ROOT/initdb-k3s.log"; exit 1; }
umount "$PGSTAGE/proc" 2>/dev/null || true
umount "$PGSTAGE/dev" 2>/dev/null || true
trap - EXIT

# Determinism overlay on the baked cluster's postgresql.conf. UNLIKE task 38
# (unix-socket-only), the server pod must accept the client pod's connection over
# TCP across the CNI — so it listens on `*` and pg_hba trusts the cluster CIDRs.
# `log_connections=on` + `%h` in the prefix logs the client's source IP — which is
# a POD IP (10.42.x.x), the witness that the path stayed INTRA-GUEST over the CNI.
cat >>"$PGSTAGE$PGDATA_REL/postgresql.conf" <<EOF

# --- task 49 determinism overlay (see guest/linux/IMPLEMENTATION.md) ---
listen_addresses = '*'           # TCP: the client pod connects across the CNI
port = 5432
unix_socket_directories = '/tmp' # writable in the container (no /run/postgresql)
fsync = on                       # instant + deterministic on RAM-backed rootfs
jit = off
log_timezone = 'UTC'
timezone = 'UTC'
log_line_prefix = '[pg %p %h] '  # %h = client host = the CLIENT POD IP (CNI witness)
log_connections = on             # logs "connection received: host=10.42.x.x" — intra-guest
log_statement = 'none'
shared_buffers = 32MB
dynamic_shared_memory_type = posix
max_connections = 16
autovacuum = off
max_wal_size = 64MB
EOF
# Trust the cluster CIDRs (pod 10.42.0.0/16, service 10.43.0.0/16) over TCP. A
# trusted single-purpose determinism gate with no external network — `host all
# all all trust` is the simplest correct rule (initdb only trusts loopback).
printf 'host all all all trust\n' >>"$PGSTAGE$PGDATA_REL/pg_hba.conf"

# Bake PGDATA into the guest rootfs as the server pod's hostPath, owned uid 999
# (postgres requires PGDATA 0700 owned by the running uid).
mkdir -p "$K3SROOT/k8s/pgdata"
cp -a "$PGSTAGE$PGDATA_REL/." "$K3SROOT/k8s/pgdata/"
chown -R 999:999 "$K3SROOT/k8s/pgdata"
chmod 0700 "$K3SROOT/k8s/pgdata"
rm -rf "$PGSTAGE" "$IMG"        # only PGDATA is baked; the staging rootfs is thrown away

# --- 5. the workload + the client wrapper (baked as hostPaths) ----------------
# The SAME workload v2 as task 37/42: each row carries a gen_random_uuid() id
# (column DEFAULT) + a clock_timestamp() column, streamed as `row|i|count|sum|
# uuid|t`. The count/sum prefix is a pure function of the loop index (the
# deterministic anchor, `row|20|20|210|`); the uuid + t are seed-derived.
{
    echo "CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);"
    i=1
    while [ "$i" -le "$WORKLOAD_N" ]; do
        echo "INSERT INTO ledger(i,t) VALUES ($i, clock_timestamp());"
        echo "SELECT 'row', i, (SELECT count(*) FROM ledger), (SELECT sum(i) FROM ledger), id, t FROM ledger WHERE i=$i;"
        i=$((i+1))
    done
} >"$K3SROOT/k8s/workload.sql"

# The client pod's command (its hostPath /client.sh). Connects to the postgres
# Service ClusterIP over the CNI (a short connect-retry covers the brief window
# between the Service getting endpoints and kube-proxy programming the DNAT),
# then runs the workload and streams the rows. K8S49 markers bracket the run.
cat >"$K3SROOT/k8s/client.sh" <<EOF
#!/bin/sh
set -u
export PGCONNECT_TIMEOUT=5 LC_ALL=C.UTF-8 PGTZ=UTC
PGHOST=$PG_CLUSTERIP PGPORT=5432 PGUSER=postgres PGDATABASE=postgres
export PGHOST PGPORT PGUSER PGDATABASE
echo "K8S49: client pod starting; target postgres Service ClusterIP \$PGHOST:\$PGPORT (over the CNI)"
i=0
until psql -q -c 'SELECT 1' >/dev/null 2>&1; do
    i=\$((i+1)); [ "\$i" -gt 600 ] && { echo "K8S49: postgres unreachable over the CNI after \$i tries"; exit 1; }
    sleep 1
done
echo "K8S49: client connected to the postgres pod over the CNI (ClusterIP \$PGHOST)"
echo "K8S49: workload begin"
psql -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql
echo "K8S49: workload end"
EOF
chmod 0755 "$K3SROOT/k8s/client.sh"

# --- 6. the k3s config + the Kubernetes manifests -----------------------------
# Trim everything the gate doesn't need (the spec): no traefik/servicelb/metrics/
# local-storage; CoreDNS off (we target the Service ClusterIP directly, no DNS);
# no network-policy/helm controllers. flannel host-gw: single-node, so all pod
# traffic is same-subnet on the cni0 bridge — host-gw avoids the vxlan device
# entirely. A fixed token removes one random input (it would be CRNG-deterministic
# anyway). A fixed node name keeps the node object reproducible.
cat >"$K3SROOT/etc/rancher/k3s/config.yaml" <<EOF
node-name: det-node
token: harmony-task49-deterministic-token
flannel-backend: host-gw
disable-network-policy: true
disable-helm-controller: true
disable:
  - traefik
  - servicelb
  - metrics-server
  - local-storage
  - coredns
EOF

# The postgres Pod + Service are baked into the server manifests dir, which k3s
# auto-applies once the apiserver is up. The client Pod is applied separately by
# k3s-init.sh AFTER the postgres pod is Ready (clean sequencing for the gate
# narrative; the client's retry loop makes it robust regardless).
cat >"$K3SROOT/var/lib/rancher/k3s/server/manifests/postgres.yaml" <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: postgres
  namespace: default
  labels: { app: postgres }
spec:
  restartPolicy: Never
  terminationGracePeriodSeconds: 2
  securityContext:
    runAsUser: 999
    runAsGroup: 999
    fsGroup: 999
  containers:
    - name: postgres
      image: docker.io/library/postgres:17
      imagePullPolicy: Never
      command: ["postgres", "-D", "/var/lib/postgresql/data"]
      env:
        - { name: LC_ALL,  value: "C.UTF-8" }
        - { name: LANG,    value: "C.UTF-8" }
        - { name: TZ,      value: "UTC" }
        - { name: PGTZ,    value: "UTC" }
      ports:
        - { containerPort: 5432 }
      readinessProbe:
        tcpSocket: { port: 5432 }
        initialDelaySeconds: 2
        periodSeconds: 3
        failureThreshold: 60
      volumeMounts:
        - { name: pgdata, mountPath: /var/lib/postgresql/data }
        - { name: shm,    mountPath: /dev/shm }
  volumes:
    - name: pgdata
      hostPath: { path: /k8s/pgdata, type: Directory }
    - name: shm
      emptyDir: { medium: Memory, sizeLimit: 256Mi }
---
apiVersion: v1
kind: Service
metadata:
  name: postgres
  namespace: default
spec:
  clusterIP: $PG_CLUSTERIP
  selector: { app: postgres }
  ports:
    - { port: 5432, targetPort: 5432, protocol: TCP }
EOF

cat >"$K3SROOT/k8s/client.yaml" <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: client
  namespace: default
  labels: { app: client }
spec:
  restartPolicy: Never
  terminationGracePeriodSeconds: 2
  containers:
    - name: client
      image: docker.io/library/postgres:17
      imagePullPolicy: Never
      command: ["/bin/sh", "/client.sh"]
      volumeMounts:
        - { name: workload, mountPath: /workload.sql }
        - { name: clientsh, mountPath: /client.sh }
  volumes:
    - name: workload
      hostPath: { path: /k8s/workload.sql, type: File }
    - name: clientsh
      hostPath: { path: /k8s/client.sh, type: File }
EOF

# --- 7. the guest /init ------------------------------------------------------
install -m 0755 "$LINUX_DIR/k3s-init.sh" "$K3SROOT/k3s-init"

# --- 8. pack the initramfs (sorted, fixed mtime, gzip -n) ---------------------
# **Ownership is PRESERVED** (no --owner=0:0): the guest-side files are root-owned
# (root created them) while /k8s/pgdata stays owned uid 999 — which the server
# pod's postgres (uid 999) needs. Ownership is a deterministic function of the
# image + initdb, so the image stays reproducible.
echo "== k3s image: packing initramfs"
find "$K3SROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$K3SROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-k3s.cpio.gz"
echo "ok: $ART_DIR/initramfs-k3s.cpio.gz ($(du -h "$ART_DIR/initramfs-k3s.cpio.gz" | cut -f1))"
