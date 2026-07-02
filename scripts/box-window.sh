#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Patched-KVM window coordinator for the determinism box. Runs ON THE BOX.
#
# The patched KVM module is box-global, so module TRANSITIONS must serialize —
# but gates between transitions may run concurrently on distinct pinned cores.
# This script turns "one gate at a time" into "one module state at a time,
# N leased gates inside it":
#
#   box-window.sh acquire <name> [--exclusive]   -> prints the leased core
#   box-window.sh release <name>                 -> last lease out reverts+verifies
#   box-window.sh status
#
# Protocol: flock on $LOCK serializes transitions and lease bookkeeping. Leases
# live in $LEASES as <name> files containing "pid core". Stale leases (dead pid)
# are swept on every acquire/release. Cores are allocated from CORES in order
# (2 first — the historical frontier core — then 1, then 3); SMT siblings stay
# idle per docs/BOX-PINNING.md. --exclusive (measurement gates: skid, seal-rate)
# waits until it is the ONLY lease and blocks joiners until released.
#
# Box-safety invariant: the window NEVER outlives its last lease — release of
# the final lease reverts to stock 1396736 and verifies, loudly. If a gate dies
# without releasing, the next acquire/release sweeps its stale lease; a window
# with zero live leases is reverted on the next invocation of any verb.
set -uo pipefail
LOCK=/root/box-window.lock
LEASES=/root/box-window-leases
EXCL_MARK=$LEASES/.exclusive
B=/root/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm
STOCK_SIZE=1396736
CORES=(2 1 3)

mkdir -p "$LEASES"

kvm_size() { lsmod | awk '$1=="kvm"{print $2}'; }

sweep_stale() { # under lock
    for f in "$LEASES"/*; do
        [ -f "$f" ] || continue
        [ "$(basename "$f")" = ".exclusive" ] && continue
        read -r pid _ < "$f" || { rm -f "$f"; continue; }
        kill -0 "$pid" 2>/dev/null || { echo "sweeping stale lease $(basename "$f") (pid $pid dead)" >&2; rm -f "$f"; }
    done
    [ -f "$EXCL_MARK" ] && { read -r epid < "$EXCL_MARK"; kill -0 "$epid" 2>/dev/null || rm -f "$EXCL_MARK"; }
}

live_leases() { find "$LEASES" -maxdepth 1 -type f ! -name '.exclusive' | wc -l | tr -d ' '; }

load_patched() {
    echo "=== window open: loading patched KVM ===" >&2
    [ "$(kvm_size)" = "$STOCK_SIZE" ] || { echo "ABORT: kvm is neither stock nor cleanly loadable (size $(kvm_size))" >&2; return 1; }
    users=$(lsmod | awk '$1=="kvm_intel"{print $3}')
    [ "${users:-0}" = "0" ] || { echo "ABORT: kvm_intel in use ($users users)" >&2; return 1; }
    rmmod kvm_intel kvm && insmod "$B/kvm.ko" && insmod "$B/kvm-intel.ko"
}

revert_stock() {
    echo "=== window close: reverting to stock KVM ===" >&2
    for t in 1 2 3 4 5 6 7 8; do rmmod kvm_intel kvm 2>/dev/null && break; sleep 2; done
    modprobe kvm 2>/dev/null; modprobe kvm_intel 2>/dev/null
    sz=$(kvm_size)
    echo "lsmod kvm = $sz (want $STOCK_SIZE)" >&2
    [ "$sz" = "$STOCK_SIZE" ] && echo "REVERT OK" >&2 || { echo "REVERT MISMATCH ($sz)!" >&2; return 1; }
}

case "${1:?usage: box-window.sh acquire|release|status ...}" in
acquire)
    NAME="${2:?acquire needs a lease name}"
    EXCL=0; [ "${3:-}" = "--exclusive" ] && EXCL=1
    while true; do
        exec 9>"$LOCK"
        flock 9
        sweep_stale
        n=$(live_leases)
        if [ "$EXCL" = 1 ] && [ "$n" -gt 0 ]; then flock -u 9; sleep 15; continue; fi
        if [ "$EXCL" = 0 ] && [ -f "$EXCL_MARK" ]; then flock -u 9; sleep 15; continue; fi
        # allocate a core not held by a live lease
        core=""
        for c in "${CORES[@]}"; do
            grep -qs " $c\$" "$LEASES"/* 2>/dev/null || { core=$c; break; }
        done
        [ -n "$core" ] || { flock -u 9; sleep 15; continue; }   # all cores leased
        if [ "$n" -eq 0 ]; then
            load_patched || { flock -u 9; exit 1; }
        fi
        echo "$PPID $core" > "$LEASES/$NAME"
        [ "$EXCL" = 1 ] && echo "$PPID" > "$EXCL_MARK"
        flock -u 9
        echo "$core"
        exit 0
    done
    ;;
release)
    NAME="${2:?release needs the lease name}"
    exec 9>"$LOCK"
    flock 9
    rm -f "$LEASES/$NAME"
    [ -f "$EXCL_MARK" ] && rm -f "$EXCL_MARK"
    sweep_stale
    if [ "$(live_leases)" -eq 0 ] && [ "$(kvm_size)" != "$STOCK_SIZE" ]; then
        revert_stock || { flock -u 9; exit 1; }
    fi
    flock -u 9
    ;;
status)
    exec 9>"$LOCK"; flock 9
    sweep_stale
    echo "kvm size: $(kvm_size) (stock=$STOCK_SIZE)"
    echo "live leases: $(live_leases)"; ls -la "$LEASES" 2>/dev/null | tail -n +2
    flock -u 9
    ;;
*) echo "unknown verb: $1" >&2; exit 1 ;;
esac
