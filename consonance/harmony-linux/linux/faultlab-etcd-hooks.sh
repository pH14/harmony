#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Hook dispatcher for the etcd bundle. One argument: the hook id from
# /bundle/etcd-<version>. Directives go to stdout, diagnostics to stderr.
set -u
. /faultlab-common.sh

ETCDCTL="$ETCDROOT/etcdctl --endpoints=127.0.0.1:2379"
JOURNAL=/run/journal

hook_put() {
    n=$(faultlab_arg faultlab.puts 200)
    i=0
    acked=0
    while [ "$i" -lt "$n" ]; do
        # The journal is the client's record of what the server ACKED. It is
        # appended only after a successful put, so a key in the journal that is
        # gone after a restart is a durability violation by definition — the
        # single-member ground truth the upstream postmortem had no tool for.
        if $ETCDCTL put "k$i" "v$i" >/dev/null 2>>/run/hook.err; then
            echo "k$i" >>"$JOURNAL"
            acked=$((acked + 1))
        fi
        i=$((i + 1))
    done
    echo "@reachable 10"
    [ "$acked" -gt 0 ] && echo "@sometimes 11"
    [ "$acked" -lt "$n" ] && echo "@sometimes 12"
    return 0
}

hook_verify() {
    [ -f "$JOURNAL" ] || return 0
    # Several instances of this hook can run at once, so the scratch files
    # are private to each; shared names let one instance's lists be compared
    # against another's and report keys as missing that were never lost.
    scratch=/run/verify.$$
    mkdir -p "$scratch"
    # The journal is copied before the server is asked, so a put that lands
    # while this hook runs is either in both lists or in neither. Read in the
    # other order, a key acknowledged between the two reads would count as
    # lost.
    sort -u "$JOURNAL" >"$scratch/acked"
    $ETCDCTL get --prefix k --keys-only >"$scratch/present.raw" 2>>/run/hook.err || {
        # An unreachable member is not an inconsistency; the oracle stays silent
        # so that a killed node cannot be mistaken for lost data.
        echo "@reachable 13"
        rm -rf "$scratch"
        return 0
    }
    grep -x 'k[0-9]*' "$scratch/present.raw" | sort -u >"$scratch/present"
    missing=$(comm -23 "$scratch/acked" "$scratch/present" | wc -l)
    rm -rf "$scratch"
    echo "@reachable 14"
    if [ "$missing" -gt 0 ]; then
        echo "@always 1 0"
        echo "missing=$missing" >>/run/hook.err
    else
        echo "@always 1 1"
    fi
    return 0
}

case "$1" in
    1) hook_put ;;
    2) hook_verify ;;
    *) echo "unknown hook $1" >&2; exit 1 ;;
esac
