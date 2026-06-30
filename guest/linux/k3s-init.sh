#!/bin/sh
# /init of the **Postgres-on-k3s workload image** (task 49). Selected by the kernel
# `rdinit=/k3s-init` cmdline param. Brings up a single-node lightweight Kubernetes
# cluster (k3s) inside the deterministic guest, then runs a CLIENT pod that makes
# calls to a POSTGRES server pod over the in-guest CNI (pod -> ClusterIP ->
# kube-proxy DNAT -> the server pod, all intra-guest), runs the task-42
# gen_random_uuid()/clock_timestamp() workload, and streams it to ttyS0.
#
# **Why this works under the deterministic VMM (the unlock — tasks 47/52/54).**
# kubelet + containerd + apiserver + scheduler + controller-manager + kube-proxy +
# flannel are all Go/multi-goroutine services that busy-spin and depend on
# preemption. The V-time LAPIC timer PREEMPTS a busy-spinning thread at the
# seed-deterministic V-time deadline (run_until), the idle-HLT resume warps to the
# next deadline (task 52), and the xAPIC MMIO is routed to the deterministic LAPIC
# model (task 54). So the Go schedulers run, timers/watches/leases fire, and the
# cluster converges — deterministically, because every preemption instant is a
# pure function of the seed. We therefore use NORMAL blocking waits (`sleep`-paced
# readiness polls): the timer now advances even while this init blocks, so a
# `sleep` actually wakes (task 52) — this is legitimate sequencing, NOT a
# preemption-dodging cooperative shim (that is banned; the k8s services run for
# real, driven by the real preemption primitive).
#
# **Serial discipline (determinism gate).** k3s' own verbose log is kept in
# /run/k3s.log (NOT streamed to ttyS0): k8s logs are full of durations/goroutine
# ordering that need not be bit-identical. ttyS0 carries only curated, deterministic
# markers (K8S49: ...) + the client pod's workload output (the row|... lines, the
# seed-derived UUIDs/timestamps) — exactly the task-38 pattern (stream the workload,
# not the debug log). `state_hash` still captures the full deterministic machine.

BB=/bin/busybox
export PATH=/usr/local/bin:/bin:/sbin
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
export HOME=/root
K3SLOG=/run/k3s.log
NODE=det-node

log()  { $BB echo "K8S49: $*"; }
kc()   { k3s kubectl "$@"; }

tail_k3s() {
    $BB echo "----- BEGIN k3s.log tail (failure diagnostics) -----"
    $BB tail -40 "$K3SLOG" 2>/dev/null
    $BB echo "----- END k3s.log tail -----"
}

# Terminal: print the seeded-CRNG witness (boot_id, identical across same-seed
# runs), GUEST_READY only on a clean success, then a forced triple-fault reboot
# (reboot=t,force on the cmdline) — the device_shutdown stall a plain poweroff
# hits once block I/O has run is bypassed (task 37/38).
finish() {
    rc=$1
    log "boot_id=$($BB cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
    if [ "$rc" = 0 ]; then
        log "result rc=0 (client workload completed over the CNI)"
        $BB echo "GUEST_READY"
    else
        log "result rc=$rc (FAILED — see /run/k3s.log)"
    fi
    $BB sync
    exec $BB reboot -f
}

# --- kernel filesystems (as runc-init.sh) ------------------------------------
$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mkdir -p /dev/shm /dev/pts /run /tmp /var/lib
$BB mount -t tmpfs tmpfs /dev/shm
$BB mount -t devpts devpts /dev/pts 2>/dev/null
$BB mount -t tmpfs tmpfs /run
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp /dev/shm
$BB chmod 0666 /dev/console

# --- kubelet/containerd data dirs on tmpfs (cAdvisor cannot stat the ramfs root) ---
# The guest root is an initramfs (ramfs, device "rootfs"), which cAdvisor (embedded
# in kubelet) cannot get filesystem stats for -> "failed to get rootfs info" aborts
# the kubelet ContainerManager (and the overlayfs imagefs stat fails the same way).
# cAdvisor DOES recognize tmpfs (statfs), so put the kubelet root-dir and containerd's
# overlayfs snapshotter dir on tmpfs BEFORE k3s starts (cAdvisor caches mounts at
# kubelet start). The pre-staged airgap images (/var/lib/rancher/k3s/agent/images)
# and server manifests (a sibling dir) stay on the root, intact.
$BB mkdir -p /var/lib/kubelet /var/lib/rancher/k3s/agent/containerd
$BB mount -t tmpfs tmpfs /var/lib/kubelet
$BB mount -t tmpfs tmpfs /var/lib/rancher/k3s/agent/containerd

# --- cgroup-v2 (unified) — kubelet/containerd manage their own subtrees -------
# Mount the unified hierarchy, move init out of the root cgroup into a leaf (so
# the root has no member procs and can delegate controllers), and enable the
# controllers in the root subtree. cpuset is absent (depends on SMP, off per the
# task-36 audit — single-vCPU has no affinity to partition); cpu/io/memory/pids
# give kubelet the controllers it needs for pod cgroups.
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
$BB mkdir -p /sys/fs/cgroup/init
$BB echo $$ > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
for c in cpu cpuset io memory pids; do
    $BB echo "+$c" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
done
$BB mount --make-rprivate / 2>/dev/null || true

# --- networking + resource prerequisites k8s expects -------------------------
$BB ip link set lo up 2>/dev/null || true
# k3s requires a default route + a node IP; the deterministic guest has no host NIC.
# Give lo a private global IP + a default route so k3s host-interface detection
# (net.ChooseHostInterface reads /proc/net/route) succeeds. --node-ip pins the choice.
NODE_IP=10.42.0.2
# Put the node IP on a real veth (MTU 1500), NOT lo (MTU 65536). flannel derives
# the VXLAN/bridge MTU from the node interface; lo's 65536 MTU makes flannel emit
# an MTU the cni0 bridge create rejects (EINVAL "invalid argument"). A veth yields
# FLANNEL_MTU=1450 -> cni0 MTU 1450 (valid). CONFIG_VETH=y in the SMP kernel.
$BB ip link add nodenet0 type veth peer name nodenet1 2>/dev/null || true
$BB ip link set nodenet1 mtu 1500 up 2>/dev/null || true
$BB ip link set nodenet0 mtu 1500 up 2>/dev/null || true
$BB ip addr add "$NODE_IP"/24 dev nodenet0 2>/dev/null || true
$BB ip route add default dev nodenet0 2>/dev/null || true
$BB echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null || true
$BB echo 1 > /proc/sys/net/ipv4/conf/all/forwarding 2>/dev/null || true
# Bridged pod traffic must traverse iptables for kube-proxy ClusterIP DNAT
# (BRIDGE_NETFILTER is built in; the knobs exist at boot).
$BB echo 1 > /proc/sys/net/bridge/bridge-nf-call-iptables 2>/dev/null || true
$BB echo 1 > /proc/sys/net/bridge/bridge-nf-call-ip6tables 2>/dev/null || true
# kubelet/containerd/apiserver open many fds + inotify watches.
# shellcheck disable=SC3045  # busybox ash's ulimit builtin supports -n
ulimit -n 1048576 2>/dev/null || true
$BB echo 8192  > /proc/sys/fs/inotify/max_user_instances 2>/dev/null || true
$BB echo 524288 > /proc/sys/fs/inotify/max_user_watches  2>/dev/null || true

log "starting k3s server ($(k3s --version 2>/dev/null | $BB head -1)) — log -> $K3SLOG (not ttyS0)"

# --- start the k3s server (the whole control plane + agent in one process) ----
# Foreground sub-processes (containerd, kubelet goroutines, apiserver, scheduler,
# controllers, kube-proxy, flannel) are now driven by V-time preemption. Its
# verbose log stays in a file; only our deterministic markers reach ttyS0.
k3s server --config /etc/rancher/k3s/config.yaml --node-ip="$NODE_IP" >"$K3SLOG" 2>&1 &
K3SPID=$!

# wait_for <max_polls> <desc> <test-cmd...>: poll cooperatively (sleep-paced;
# the timer wakes us — task 52) until <test-cmd> succeeds. Prints only the
# transition (deterministic), not per-iteration noise. Returns 1 on timeout.
wait_for() {
    max=$1; desc=$2; shift 2
    n=0
    until "$@" >/dev/null 2>&1; do
        n=$((n+1))
        if [ "$n" -ge "$max" ]; then
            log "TIMEOUT waiting for $desc after $n polls"
            return 1
        fi
        # k3s still alive?
        $BB kill -0 "$K3SPID" 2>/dev/null || { log "k3s server EXITED while waiting for $desc"; return 1; }
        $BB sleep 2
    done
    log "ready: $desc"
    return 0
}

# 1. apiserver answers /readyz.
wait_for 600 "apiserver /readyz" sh -c 'k3s kubectl get --raw=/readyz' || { tail_k3s; finish 1; }

# 2. the node reaches Ready (the single-node cluster is up).
wait_for 600 "node/$NODE Ready" \
    sh -c "k3s kubectl get node $NODE -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}' | grep -qx True" \
    || { tail_k3s; finish 1; }
log "CLUSTER_UP single-node k3s cluster is Ready"

# 3. the postgres Pod (auto-applied from the server manifests dir) reaches Ready.
wait_for 600 "pod/postgres Running" \
    sh -c "k3s kubectl get pod postgres -o jsonpath='{.status.phase}' | grep -qx Running" \
    || { kc describe pod postgres 2>/dev/null | $BB tail -30; tail_k3s; finish 1; }
wait_for 600 "pod/postgres Ready" \
    sh -c "k3s kubectl get pod postgres -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}' | grep -qx True" \
    || { kc describe pod postgres 2>/dev/null | $BB tail -30; tail_k3s; finish 1; }
log "POSTGRES_READY the postgres pod is Running and accepting connections"

# 4. apply the client Pod and let it call the postgres pod over the CNI.
kc apply -f /k8s/client.yaml >/dev/null 2>&1
wait_for 600 "pod/client scheduled" \
    sh -c "k3s kubectl get pod client -o jsonpath='{.status.phase}' | grep -qE 'Running|Succeeded|Failed'" \
    || { tail_k3s; finish 1; }

# 5. wait for the client to terminate (Succeeded/Failed), then stream its log.
wait_for 600 "pod/client terminated" \
    sh -c "k3s kubectl get pod client -o jsonpath='{.status.phase}' | grep -qE 'Succeeded|Failed'" \
    || { tail_k3s; finish 1; }

CLIENT_PHASE=$(kc get pod client -o jsonpath='{.status.phase}' 2>/dev/null)
log "client pod terminated: phase=$CLIENT_PHASE"

# --- the deterministic payload: the client pod's workload output to ttyS0 -----
# (raw stdout, no --timestamps: exactly the K8S49 markers + the row|... lines.)
$BB echo "----- BEGIN client pod log (the intra-guest workload output) -----"
kc logs client 2>/dev/null
$BB echo "----- END client pod log -----"

# --- intra-guest CNI witnesses (deterministic: pod IPs + the source-IP log) ---
# Pod IPs are in the pod CIDR (10.42.0.0/16), assigned by the host-local IPAM
# (sequential, deterministic). The postgres pod's connection log records the
# CLIENT's source IP (%h) — a POD IP — proving the path stayed intra-guest over
# the CNI (no host networking; pv-net unused).
PG_IP=$(kc get pod postgres -o jsonpath='{.status.podIP}' 2>/dev/null)
CL_IP=$(kc get pod client   -o jsonpath='{.status.podIP}' 2>/dev/null)
log "CNI pod IPs: postgres=$PG_IP client=$CL_IP (pod CIDR 10.42.0.0/16, intra-guest)"
$BB echo "----- BEGIN postgres connection log (source IP = client pod IP) -----"
kc logs postgres 2>/dev/null | $BB grep -E 'connection (received|authorized)' | $BB head -8
$BB echo "----- END postgres connection log -----"

if [ "$CLIENT_PHASE" = "Succeeded" ]; then
    finish 0
else
    finish 1
fi
