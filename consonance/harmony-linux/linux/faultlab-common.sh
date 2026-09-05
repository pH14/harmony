# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=sh
# Shared guest-side setup for the fault-library workload images. Sourced by
# each /rdinit script before it hands control to the fault agent.

# faultlab_arg <key> <default>: read `key=value` from the kernel command line.
# The command line is the only channel a campaign has for turning image knobs
# without rebuilding, so every tunable that a run may vary goes through it.
# /proc/cmdline is read per call because hook scripts run as their own
# processes, long after and independently of whoever mounted /proc.
faultlab_arg() {
    # shellcheck disable=SC2013  # the command line is whitespace-separated words
    _v=$(for w in $(cat /proc/cmdline 2>/dev/null); do
        case "$w" in "$1"=*) echo "${w#*=}" ;; esac
    done | tail -1)
    if [ -z "$_v" ]; then echo "$2"; else echo "$_v"; fi
}

faultlab_mount_base() {
    mount -t proc  none /proc 2>/dev/null
    mount -t sysfs none /sys  2>/dev/null
    # The kernel only auto-mounts devtmpfs when it mounts a real root, never for
    # an initramfs, so /dev holds just the baked console until this runs. Without
    # it there is no /dev/urandom for the workloads and no /dev/harmony for the
    # fault agent's transport.
    mount -t devtmpfs none /dev 2>/dev/null
    mount -t tmpfs none /tmp  2>/dev/null
    mount -t tmpfs none /run  2>/dev/null
    mkdir -p /run /tmp
    # PostgreSQL allocates its main shared memory as a POSIX segment, which
    # needs a real /dev/shm; devtmpfs does not provide one.
    mkdir -p /dev/shm
    mount -t tmpfs none /dev/shm 2>/dev/null
    # Nodes run as their own uid and put their sockets here; a fresh tmpfs is
    # mode 755, which would leave a non-root node unable to create one.
    chmod 1777 /tmp /run /dev/shm
    # Both workloads talk to themselves over loopback: etcd serves its client
    # URL there and PostgreSQL's statistics collector needs a working socket.
    ip link set lo up 2>/dev/null || ifconfig lo up 2>/dev/null
}

# Serial is compared byte for byte between control runs, so nothing that
# carries a clock, a pid or a duration may reach it. Node and hook stderr goes
# to files under /run; only the lines below and the hook directives are
# printed.
faultlab_say() { echo "FL: $*"; }

faultlab_poweroff() {
    sync
    poweroff -f
    # poweroff returns on the halt-instead-of-power-off path; idle deliberately.
    while true; do sleep 60; done
}

# faultlab_control <hook-script> <id...>: the no-fault driver. Starts the node,
# waits for ready, runs the listed hooks in order and halts. This is the
# nominal control harness — it reproduces exactly what the fault agent would
# drive with an empty standing-fault list, without depending on the agent.
faultlab_control() {
    _hooks=$1
    shift
    "$FAULTLAB_NODE" >/run/node.out 2>&1 &
    _t=0
    until "$FAULTLAB_READY" >/dev/null 2>&1; do
        _t=$((_t + 1))
        if [ "$_t" -gt 60 ]; then
            faultlab_say "ready timeout; node output follows"
            cat /run/node.out 2>/dev/null
            faultlab_poweroff
        fi
        sleep 1
    done
    faultlab_say "ready"
    for _h in "$@"; do
        faultlab_say "hook $_h start"
        "$_hooks" "$_h"
        faultlab_say "hook $_h exit $?"
    done
    faultlab_say "control done"
    faultlab_poweroff
}

# faultlab_agent <bundle>: the campaign path. Installs the bundle and hands off
# to the fault agent, which owns node lifecycle and hook dispatch from there.
faultlab_agent() {
    mkdir -p /etc/harmony
    cp "$1" /etc/harmony/bundle
    if [ ! -x /fault-agent ]; then
        faultlab_say "no /fault-agent in this image"
        faultlab_poweroff
    fi
    # /run is a tmpfs, so the agent's hook output files never touch the
    # workload's storage and cannot perturb what a fault is being searched over.
    exec /fault-agent --bundle /etc/harmony/bundle --hook-dir /run/fault-agent
}
