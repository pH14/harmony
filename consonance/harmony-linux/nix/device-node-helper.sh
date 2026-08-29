#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Create the two build-time PostgreSQL device nodes from outside the user
# namespace used by a path-backed Nix store. The caller is already inside the
# Consonance cgroup and holds both coordination locks for this helper's entire
# lifetime.
set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "usage: device-node-helper.sh TMP_ROOT REQUEST_FIFO ACK_FIFO" >&2
    exit 2
fi
tmp_root=$(realpath -e "$1")
request=$2
ack=$3
[ -p "$request" ] && [ -p "$ack" ] || {
    echo "FAIL: device-node helper endpoints must be FIFOs" >&2
    exit 1
}

for spec in 'null 1 3' 'urandom 1 9'; do
    read -r name expected_major expected_minor <<<"$spec"
    IFS=' ' read -r path major minor <"$request"
    parent=$(realpath -e "$(dirname "$path")")
    case "$parent" in
        "$tmp_root"/harmony-nix-guest.*/build/arm64-postgres-root/container/dev) ;;
        *)
            printf 'REJECT path\n' >"$ack"
            echo "FAIL: rejected device-node path: $path" >&2
            exit 1
            ;;
    esac
    [ "$path" = "$parent/$name" ] \
        && [ "$major" = "$expected_major" ] \
        && [ "$minor" = "$expected_minor" ] || {
            printf 'REJECT identity\n' >"$ack"
            echo "FAIL: rejected device-node identity: $path $major $minor" >&2
            exit 1
        }
    [ ! -e "$path" ] || {
        printf 'REJECT exists\n' >"$ack"
        echo "FAIL: device-node target already exists: $path" >&2
        exit 1
    }
    /usr/bin/mknod -m 0666 "$path" c "$major" "$minor"
    [ -c "$path" ] && [ "$(stat -c '%t:%T' "$path")" = "$major:$minor" ] || {
        printf 'REJECT verify\n' >"$ack"
        echo "FAIL: created device-node verification failed: $path" >&2
        exit 1
    }
    printf 'OK %s\n' "$path" >"$ack"
done
