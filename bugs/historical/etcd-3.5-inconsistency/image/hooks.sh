#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

journal=/tmp/etcd/journal/acked

ctl() {
  ETCDCTL_API=3 /opt/etcd/etcdctl --endpoints=http://127.0.0.1:2379 "$@"
}

writer() {
  worker=$1
  i=1
  while [ "$i" -le 2000 ]; do
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
    setsid -f "$0" worker 1 >/dev/null 2>&1
    setsid -f "$0" worker 2 >/dev/null 2>&1
    setsid -f "$0" worker 3 >/dev/null 2>&1
    setsid -f "$0" worker 4 >/dev/null 2>&1
    echo '@reachable 10'
    ;;
  2)
    [ -s "${journal}" ] || exit 0
    # A failed read means the member is still down or restarting. That is not
    # evidence of corruption; only a successful readback can publish a verdict.
    ctl endpoint health >/dev/null 2>&1 || exit 0
    snapshot=${journal}.$$
    expected=${snapshot}.expected
    actual_raw=${snapshot}.actual.raw
    actual=${snapshot}.actual
    trap 'rm -f "${snapshot}" "${expected}" "${actual_raw}" "${actual}"' EXIT
    cp "${journal}" "${snapshot}" 2>/dev/null || exit 0
    # Keep only complete, workload-shaped records. The journal is appended by
    # detached workers, so its final line can be a partial write.
    awk -F '	' '
      $1 ~ /^museum\/[1-4]\/key-[0-9]+$/ &&
      $2 ~ /^value-[1-4]-[0-9]+$/ { print $1 "\t" $2 }
    ' "${snapshot}" | LC_ALL=C sort >"${expected}"
    [ -s "${expected}" ] || exit 0

    # One prefix read replaces one etcdctl process and RPC per journal row.
    # Convert etcdctl's key/value line pairs to the same canonical form as the
    # journal, then check only the acknowledged subset. Extra keys can be
    # present because the workers may append a journal record after a put.
    ctl get museum/ --prefix >"${actual_raw}" 2>/dev/null || exit 0
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
