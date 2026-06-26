#!/bin/sh
# /init of the **Postgres-in-Docker workload image** (task 38). Brings up the
# kernel filesystems + cgroup-v2, starts a real **dockerd** (static bundle:
# dockerd + containerd + containerd-shim-runc-v2 + runc), `docker load`s the
# baked **official postgres image** (no registry pull), runs it with
# `docker run --network none`, drives the SAME fixed insert/select workload as
# task 37 against the containerized DB over its local unix socket (via
# `docker exec`), streams the container's + the loop's stdout/stderr to ttyS0,
# and reaches a clean deterministic terminal. Every byte printed is part of the
# deterministic-twice golden — see guest/linux/IMPLEMENTATION.md for the
# determinism closure (the Go-runtime AT_RANDOM/getrandom → seeded-CRNG path,
# cgroup/vfs assembly, all a pure function of V-time + the seeded stream).
#
# Three consonance-VMM realities shape the control flow (inherited from task 37,
# see IMPLEMENTATION.md), and one is new for the container stack:
#   * The VMM terminates on the FIRST guest HLT and freezes V-time while idle, so
#     the guest must NEVER go fully idle until the deliberate terminal. dockerd /
#     containerd / runc are large Go programs with quiet windows where every
#     goroutine parks on a timer; a lowest-priority KEEPALIVE busy-loop keeps
#     something runnable so the periodic V-time tick keeps firing (it preempts
#     the keepalive the instant docker is runnable, so docker still progresses).
#   * Readiness/shutdown are awaited COOPERATIVELY (a blocking docker round-trip
#     or the shell's `wait`), never by `sleep` (the sleeper never wakes).
#   * `poweroff` strands in device_shutdown once block I/O has run; we `reboot -f`
#     and the cmdline's `reboot=t,force` makes it a clean triple-fault terminal.

BB=/bin/busybox
export PATH=/usr/local/bin:/bin:/sbin
STORAGE_DRIVER=vfs                 # vfs on tmpfs (see build-docker-image.sh)
PGIMG=postgres:17
CNAME=pg

log() { $BB echo "DK38: $*"; }

# --- kernel filesystems ------------------------------------------------------
$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mkdir -p /dev/shm /dev/pts /run /tmp /var/lib/docker
$BB mount -t tmpfs tmpfs /dev/shm
$BB mount -t devpts devpts /dev/pts 2>/dev/null
$BB mount -t tmpfs tmpfs /run
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp /dev/shm
$BB chmod 0666 /dev/console      # let docker/container children reopen the console

# --- cgroup-v2 (unified) — dockerd/containerd/runc require it ----------------
# Mount the unified hierarchy, move init out of the root cgroup (so the root has
# no member processes and can delegate controllers), and enable the controllers
# docker needs in the root subtree. cpuset is absent (it depends on SMP, which
# the determinism overlay keeps off — see task 36 audit); runc degrades over it.
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
$BB mkdir -p /sys/fs/cgroup/init
$BB echo $$ > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
for c in cpu io memory pids; do
    $BB echo "+$c" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
done

# --- storage: vfs on a tmpfs /var/lib/docker (RAM-backed, no overlay/ext4) ----
$BB mount -t tmpfs -o size=8g tmpfs /var/lib/docker

# --- KEEPALIVE: keep the guest non-idle (the first HLT is the VMM terminal) ----
# A lowest-priority busy loop. Its iteration count is a deterministic function of
# the V-time docker spends waiting (same shape as task 37's psql retry loop), so
# it adds no nondeterminism. `nice` if busybox has it, else plain (wakeup
# preemption still lets a runnable docker goroutine preempt it). Reaped by the
# terminal reboot.
# `while :; do :; done` is a pure-builtin spin (no fork churn). nice it to the
# lowest priority when busybox has the applet, else plain.
if $BB nice -n 19 $BB true 2>/dev/null; then
    $BB nice -n 19 $BB sh -c 'while :; do : ; done' &
else
    $BB sh -c 'while :; do : ; done' &
fi
KEEPALIVE=$!
log "keepalive pid=$KEEPALIVE"

# --- helpers -----------------------------------------------------------------
docker_alive() { docker ps --format '{{.Names}}' 2>/dev/null | $BB grep -qx "$CNAME"; }

# --- start dockerd -----------------------------------------------------------
# --bridge=none --iptables=false drop the daemon's default-bridge/netfilter
# surface entirely (single-node has no network; the workload uses the local
# socket). dockerd finds containerd/runc/the shim via PATH. Logs to ttyS0.
log "starting dockerd $(dockerd --version 2>/dev/null | $BB head -1)"
dockerd \
    --data-root=/var/lib/docker \
    --storage-driver="$STORAGE_DRIVER" \
    --bridge=none --iptables=false --ip6tables=false \
    --host=unix:///run/docker.sock 2>&1 &
DOCKERD=$!

# Cooperative readiness wait: each `docker version` round-trips the daemon socket
# and blocks/yields until dockerd answers; the retry count is a deterministic
# function of dockerd's V-time-to-ready. Probe output to /dev/null.
export DOCKER_HOST=unix:///run/docker.sock
log "waiting for dockerd"
until docker version >/dev/null 2>&1; do
    $BB kill -0 "$DOCKERD" 2>/dev/null || { log "FATAL: dockerd exited before ready"; break; }
done
log "dockerd is up"

# --- load the baked official postgres image (no registry pull) ----------------
log "docker load < /postgres-image.tar"
docker load -i /postgres-image.tar 2>&1
log "images:"; docker images --format '{{.Repository}}:{{.Tag}} {{.ID}}' 2>&1

# --- run the container, --network none, streaming its logs to ttyS0 -----------
log "docker run --network none $PGIMG"
docker run -d --name "$CNAME" --network none \
    -e POSTGRES_HOST_AUTH_METHOD=trust \
    "$PGIMG" 2>&1
docker logs -f "$CNAME" 2>&1 &        # live container stdout/stderr -> ttyS0
LOGJOB=$!

# Wait for the REAL server, not the entrypoint's transient init server. The
# official image runs initdb against a temporary unix-socket server, then prints
# "PostgreSQL init process complete; ready for start up." and starts the final
# server. Gate on that marker (PGDATA is fresh each boot, so init always runs),
# then on pg_isready — so we never drive the temp server and race its shutdown.
log "waiting for postgres init to complete"
until docker logs "$CNAME" 2>&1 | $BB grep -q 'init process complete'; do
    docker_alive || { log "FATAL: container exited during init"; break; }
done
log "waiting for postgres to accept connections"
until docker exec -u postgres "$CNAME" pg_isready -q >/dev/null 2>&1; do
    docker_alive || { log "FATAL: container exited before ready"; break; }
done
log "postgres ready in container"

# --- drive the SAME insert/select workload as task 37, over the local socket --
# psql runs INSIDE the container (docker exec) as the postgres user, connecting
# to the cluster's unix socket; the workload SQL is fed on stdin. Values are a
# pure function of the loop index, so the row|… output is a deterministic
# function of the seed (identical to task 37's golden rows).
log "workload begin"
docker exec -i -u postgres "$CNAME" \
    psql -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -d postgres < /workload.sql 2>&1
log "workload end"

# --- prove the seeded-CRNG / Go-AT_RANDOM path is deterministic ---------------
# The container id is drawn from the kernel CRNG (getrandom) and boot_id is the
# CRNG's own UUID; both being bit-identical across two same-seed runs is the
# explicit proof that the AT_RANDOM/getrandom path the Go runtimes seed from is
# fully on the seeded stream (the overall bit-identical serial proves the Go
# map-iteration order that rides on it, too). See IMPLEMENTATION.md.
log "container_id=$(docker inspect --format '{{.Id}}' "$CNAME" 2>/dev/null)"
log "boot_id=$($BB cat /proc/sys/kernel/random/boot_id 2>/dev/null)"

# --- cooperative shutdown ----------------------------------------------------
log "stopping container"
docker stop -t 20 "$CNAME" >/dev/null 2>&1
wait "$LOGJOB" 2>/dev/null            # shell builtin (yields the vCPU to the stop)
log GUEST_READY
$BB sync

# Force a triple-fault reboot terminal (reboot=t,force) — bypasses the
# device_shutdown stall a plain poweroff hits once block I/O has run.
exec $BB reboot -f
