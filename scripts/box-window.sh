#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Patched-KVM window coordinator for the determinism box. Runs ON THE BOX.
#
# The patched KVM module is box-global, so module TRANSITIONS must serialize —
# but gates between transitions may run concurrently on distinct pinned cores.
# This script turns "one gate at a time" into "one module state at a time,
# N leased gates inside it":
#
#   box-window.sh acquire <name> [--exclusive] [--ttl <seconds>]  -> prints the leased core
#   box-window.sh renew   <name> [<seconds>]                      -> extend a lease's deadline
#   box-window.sh release <name>                                  -> last lease out reverts+verifies
#   box-window.sh status
#
# Protocol: flock on $LOCK serializes transitions and lease bookkeeping. Leases
# live in $LEASES as <name> files containing "deadline pid core". A lease is LIVE
# while `now < deadline` OR its recorded pid is still alive; it is stale (and
# swept on every verb) only once BOTH have lapsed. Cores are allocated from CORES
# in order (2 first — the historical frontier core — then 1, then 3); SMT
# siblings stay idle per docs/BOX-PINNING.md. --exclusive (measurement gates:
# skid, seal-rate) waits until it is the ONLY lease and blocks joiners until
# released.
#
# WHY TIME + PID, NOT PID ALONE (the tasks/164 / hm-nvwx fix). Every caller
# reaches the box as `ssh <box> '<one-shot command>'`, so the *natural* pattern
#
#     ssh <box> 'box-window.sh acquire t157'   # this shell exits immediately
#
# runs acquire in a shell that dies the instant the command returns. A lease keyed
# on that shell's pid is stale the moment it exists: the next verb sweeps it, and a
# window with zero live leases reverts — rmmod'ing the patched module out from
# under a live campaign in another ssh session. Making a lease live for a TTL
# regardless of pid closes that hole: a fresh acquire is valid whether or not the
# ssh shell survives. The pid is retained only to *extend* liveness for callers
# that legitimately hold a lease inside one long-lived box process (the 3-wide
# campaign orchestrators) — it can lengthen a lease's life but, unlike before, its
# death can never cut a lease short inside the lease's TTL. Renew before the TTL
# runs out for gates longer than it; a crashed gate self-heals when the TTL lapses.
#
# Box-safety invariant: the window NEVER outlives its last LIVE lease — release of
# the final lease (or expiry of the last one) reverts to stock 1396736 and verifies,
# loudly, on the next verb. A window with zero live leases is reverted on the next
# invocation of any verb.
#
# Test/CI hooks (all default to the production values; unset in normal box use, so
# behaviour on the box is byte-identical to reading them as constants):
#   BOX_WINDOW_LEASES / BOX_WINDOW_LOCK  - relocate bookkeeping (hermetic tests)
#   BOX_WINDOW_KVM_B                     - patched-module .ko directory
#   BOX_WINDOW_STOCK_SIZE                - stock kvm lsmod size
#   BOX_WINDOW_TTL                       - default lease TTL in seconds
#   BOX_WINDOW_CORES                     - space-separated core allocation order
#   BOX_WINDOW_NOW                       - override "now" (epoch seconds) for time tests
#   BOX_WINDOW_PID                       - pid recorded on acquire ("-" = none); defaults to $PPID
set -uo pipefail
LOCK="${BOX_WINDOW_LOCK:-/root/box-window.lock}"
LEASES="${BOX_WINDOW_LEASES:-/root/box-window-leases}"
EXCL_MARK="$LEASES/.exclusive"
B="${BOX_WINDOW_KVM_B:-/root/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm}"
STOCK_SIZE="${BOX_WINDOW_STOCK_SIZE:-1396736}"
TTL_DEFAULT="${BOX_WINDOW_TTL:-1800}"
read -r -a CORES <<< "${BOX_WINDOW_CORES:-2 1 3}"

mkdir -p "$LEASES"

now() { echo "${BOX_WINDOW_NOW:-$(date +%s)}"; }
is_int() { case "${1:-}" in ''|*[!0-9]*) return 1;; *) return 0;; esac; }

kvm_size() { lsmod | awk '$1=="kvm"{print $2}'; }

# A lease file is LIVE while now < deadline OR its recorded pid is still alive.
# Returns 0 if live, 1 otherwise. (core is the third field, unused here.)
lease_live() { # $1=lease file
    local f="$1" deadline pid _core
    read -r deadline pid _core < "$f" || return 1
    is_int "$deadline" || return 1          # malformed → not live (swept)
    [ "$deadline" -gt "$(now)" ] && return 0
    is_int "$pid" && kill -0 "$pid" 2>/dev/null && return 0
    return 1
}

sweep_stale() { # under lock: drop leases that are neither time-live nor pid-live
    local f b
    for f in "$LEASES"/*; do
        [ -f "$f" ] || continue
        b=$(basename "$f")
        [ "$b" = ".exclusive" ] && continue
        lease_live "$f" || { echo "sweeping stale lease $b" >&2; rm -f "$f"; }
    done
    # the exclusive mark names its holding lease; drop it if that lease is gone
    if [ -f "$EXCL_MARK" ]; then
        read -r xname < "$EXCL_MARK" || xname=""
        { [ -n "$xname" ] && [ -f "$LEASES/$xname" ]; } || rm -f "$EXCL_MARK"
    fi
}

live_leases() { find "$LEASES" -maxdepth 1 -type f ! -name '.exclusive' | wc -l | tr -d ' '; }

core_held() { # $1=core ; true if a (post-sweep, hence live) lease holds it
    local f b _d _p c
    for f in "$LEASES"/*; do
        [ -f "$f" ] || continue
        b=$(basename "$f")
        [ "$b" = ".exclusive" ] && continue
        read -r _d _p c < "$f" || continue
        [ "$c" = "$1" ] && return 0
    done
    return 1
}

load_patched() {
    echo "=== window open: loading patched KVM ===" >&2
    [ "$(kvm_size)" = "$STOCK_SIZE" ] || { echo "ABORT: kvm is neither stock nor cleanly loadable (size $(kvm_size))" >&2; return 1; }
    users=$(lsmod | awk '$1=="kvm_intel"{print $3}')
    [ "${users:-0}" = "0" ] || { echo "ABORT: kvm_intel in use ($users users)" >&2; return 1; }
    rmmod kvm_intel kvm && insmod "$B/kvm.ko" && insmod "$B/kvm-intel.ko"
}

revert_stock() {
    echo "=== window close: reverting to stock KVM ===" >&2
    for _ in 1 2 3 4 5 6 7 8; do rmmod kvm_intel kvm 2>/dev/null && break; sleep 2; done
    modprobe kvm 2>/dev/null; modprobe kvm_intel 2>/dev/null
    sz=$(kvm_size)
    echo "lsmod kvm = $sz (want $STOCK_SIZE)" >&2
    if [ "$sz" = "$STOCK_SIZE" ]; then echo "REVERT OK" >&2; else echo "REVERT MISMATCH ($sz)!" >&2; return 1; fi
}

case "${1:?usage: box-window.sh acquire|renew|release|status ...}" in
acquire)
    NAME="${2:?acquire needs a lease name}"
    EXCL=0; TTL="$TTL_DEFAULT"
    shift 2 || true
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --exclusive) EXCL=1 ;;
            --ttl) shift; TTL="${1:?--ttl needs seconds}" ;;
            *) echo "acquire: unknown option '$1'" >&2; exit 2 ;;
        esac
        shift
    done
    is_int "$TTL" || { echo "acquire: --ttl must be integer seconds" >&2; exit 2; }
    LEASE_PID="${BOX_WINDOW_PID:-$PPID}"
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
            core_held "$c" || { core=$c; break; }
        done
        [ -n "$core" ] || { flock -u 9; sleep 15; continue; }   # all cores leased
        if [ "$n" -eq 0 ]; then
            load_patched || { flock -u 9; exit 1; }
        fi
        echo "$(( $(now) + TTL )) $LEASE_PID $core" > "$LEASES/$NAME"
        [ "$EXCL" = 1 ] && echo "$NAME" > "$EXCL_MARK"
        flock -u 9
        echo "$core"
        exit 0
    done
    ;;
renew)
    NAME="${2:?renew needs the lease name}"
    TTL="${3:-$TTL_DEFAULT}"
    is_int "$TTL" || { echo "renew: seconds must be an integer" >&2; exit 2; }
    exec 9>"$LOCK"; flock 9
    sweep_stale
    if [ -f "$LEASES/$NAME" ]; then
        read -r _ pid core < "$LEASES/$NAME"
        echo "$(( $(now) + TTL )) $pid $core" > "$LEASES/$NAME"
        echo "renewed lease $NAME for ${TTL}s on core $core" >&2
        flock -u 9
    else
        echo "renew: no live lease '$NAME' (expired or never acquired) — re-acquire" >&2
        flock -u 9; exit 1
    fi
    ;;
release)
    NAME="${2:?release needs the lease name}"
    exec 9>"$LOCK"
    flock 9
    rm -f "$LEASES/$NAME"
    # drop the exclusive mark only if it named the lease we just released
    if [ -f "$EXCL_MARK" ]; then
        read -r xname < "$EXCL_MARK" || xname=""
        [ "$xname" = "$NAME" ] && rm -f "$EXCL_MARK"
    fi
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
    echo "live leases: $(live_leases)"
    nowv=$(now)
    for f in "$LEASES"/*; do
        [ -f "$f" ] || continue
        b=$(basename "$f")
        [ "$b" = ".exclusive" ] && continue
        read -r deadline pid core < "$f" || continue
        rem="-"; is_int "$deadline" && rem=$(( deadline - nowv ))
        echo "  $b: core $core, pid $pid, ${rem}s to deadline"
    done
    [ -f "$EXCL_MARK" ] && { read -r xname < "$EXCL_MARK"; echo "  exclusive holder: $xname"; }
    flock -u 9
    ;;
*) echo "unknown verb: $1" >&2; exit 1 ;;
esac
