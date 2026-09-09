#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

journal=/tmp/etcd/journal/acked

ctl() {
  /opt/etcd/etcdctl --endpoints=http://127.0.0.1:2379 "$@"
}

writer() {
  worker=$1
  i=1
  # Keep the acknowledged journal bounded so the complete prefix read fits
  # within one ordinary etcdctl request even on the slowest guest backend.
  while [ "$i" -le 256 ]; do
    key="museum/${worker}/key-${i}"
    value="value-${worker}-${i}"
    # Keep pressure on the apply path across a node kill. A failed request is
    # not journaled and is retried after the restart, so the journal contains
    # only writes the client actually acknowledged.
    if ctl put "$key" "$value" >/dev/null 2>&1; then
      printf '%s\t%s\n' "$key" "$value" >>"${journal}"
      i=$((i + 1))
    else
      sleep 0.001
    fi
  done
}

if [ "${1:-}" = worker ]; then
  writer "$2"
  exit 0
fi

case "$1" in
  1)
    # Keep several independent clients applying entries while Harmony is free
    # to kill the node. Every acknowledged put is journaled outside etcd; hook
    # 2 compares this client record with the recovered bbolt contents.
    # `setsid -f` double-forks the workers so this one-shot hook returns while
    # they keep the apply path busy for later Kill/Restart actions.
    # Establish the oracle's non-empty acknowledged-write precondition before
    # detaching the pressure writers. On slow TCG guests, a detached etcdctl
    # process may not complete before the first scheduled restart otherwise.
    if ctl put museum/0/key-0 value-0-0 >/dev/null 2>&1; then
      printf 'museum/0/key-0\tvalue-0-0\n' >>"${journal}"
    else
      exit 0
    fi
    setsid -f "$0" worker 1 >/dev/null 2>&1
    setsid -f "$0" worker 2 >/dev/null 2>&1
    setsid -f "$0" worker 3 >/dev/null 2>&1
    setsid -f "$0" worker 4 >/dev/null 2>&1
    echo '@reachable 10'
    ;;
  2)
    echo '@sometimes 12'
    [ -s "${journal}" ] || exit 0
    echo '@sometimes 13'
    ready_count=$(grep -c 'ready to serve client requests' /tmp/etcd/node.log 2>/dev/null || true)
    if [ "${ready_count}" -ge 2 ]; then
      echo '@sometimes 17'
    else
      echo '@sometimes 18'
    fi
    snapshot=${journal}.$$
    expected=${snapshot}.expected
    actual_raw=${snapshot}.actual.raw
    actual=${snapshot}.actual
    trap 'rm -f "${snapshot}" "${expected}" "${actual_raw}" "${actual}"' EXIT
    cp "${journal}" "${snapshot}" 2>/dev/null || exit 0
    # Keep only complete, workload-shaped records. The journal is appended by
    # detached workers, so its final line can be a partial write.
    awk -F '	' '
      $1 ~ /^museum\/[0-4]\/key-[0-9]+$/ &&
      $2 ~ /^value-[0-4]-[0-9]+$/ { print $1 "\t" $2 }
    ' "${snapshot}" | LC_ALL=C sort >"${expected}"
    [ -s "${expected}" ] || exit 0
    echo '@sometimes 15'

    # One prefix read replaces one etcdctl process and RPC per journal row.
    # Convert etcdctl's key/value line pairs to the same canonical form as the
    # journal, then check only the acknowledged subset. Extra keys can be
    # present because the workers may append a journal record after a put.
    # A restarted member can log that it is ready before its client listener
    # accepts this request. Retry the read itself rather than relying on a
    # separate health RPC that does not prove the data read will complete.
    readback=0
    attempt=1
    while [ "$attempt" -le 3 ]; do
      : >"${actual_raw}"
      if ctl get museum/ --prefix >"${actual_raw}" 2>/dev/null; then
        readback=1
        break
      fi
      sleep 0.01
      attempt=$((attempt + 1))
    done
    [ "$readback" -eq 1 ] || exit 0
    echo '@sometimes 16'
    awk '
      NR % 2 == 1 { key = $0; next }
      { print key "\t" $0 }
    ' "${actual_raw}" | LC_ALL=C sort >"${actual}"
    failed=0
    missing=$(comm -23 "${expected}" "${actual}")
    [ -z "${missing}" ] || failed=1
    echo '@reachable 11'
    if [ "$failed" -eq 0 ]; then
      echo '@always 1 1'
    else
      echo '@always 1 0'
    fi
    ;;
  *)
    echo "unknown hook $1" >&2
    exit 2
    ;;
esac
