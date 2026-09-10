#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

journal=/tmp/etcd/journal/acked
writers_started=/tmp/etcd/journal/writers-started
cluster_endpoints='http://127.0.0.1:2379,http://127.0.0.1:2381,http://127.0.0.1:2383'
member_endpoint_1=http://127.0.0.1:2379
member_endpoint_2=http://127.0.0.1:2381
member_endpoint_3=http://127.0.0.1:2383

ctl() {
  ETCDCTL_API=3 /opt/etcd/etcdctl --endpoints="${cluster_endpoints}" "$@"
}

ctl_member() {
  endpoint=$1
  shift
  ETCDCTL_API=3 /opt/etcd/etcdctl --endpoints="${endpoint}" "$@"
}

writer() {
  worker=$1
  i=1
  while :; do
    key="museum/${worker}/key-${i}"
    value="value-${worker}-${i}"
    # Keep pressure on the apply path across a node kill. A failed request is
    # not journaled and is retried after the restart, so the journal contains
    # only writes the client actually acknowledged.
    if ctl put "$key" "$value" >/dev/null 2>&1; then
      printf '%s\t%s\n' "$key" "$value" >>"${journal}"
      i=$((i + 1))
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
    # Search may draw this hook more than once. Only its first invocation owns
    # the writers: starting them again from key 1 could repair a lost key and
    # would append duplicate expectations to the external journal.
    if mkdir "${writers_started}" 2>/dev/null; then
      setsid -f "$0" worker 1 >/dev/null 2>&1
      setsid -f "$0" worker 2 >/dev/null 2>&1
      setsid -f "$0" worker 3 >/dev/null 2>&1
      setsid -f "$0" worker 4 >/dev/null 2>&1
    fi
    echo '@reachable 10'
    ;;
  2)
    [ -s "${journal}" ] || exit 0
    snapshot=${journal}.$$
    expected=${snapshot}.expected
    actual_raw_prefix=${snapshot}.actual.
    trap 'rm -f "${snapshot}" "${expected}" "${actual_raw_prefix}"*' EXIT
    cp "${journal}" "${snapshot}" 2>/dev/null || exit 0
    # Keep only complete, workload-shaped records. The journal is appended by
    # detached workers, so its final line can be a partial write.
    awk -F '	' '
      $1 ~ /^museum\/[1-4]\/key-[0-9]+$/ &&
      $2 ~ /^value-[1-4]-[0-9]+$/ { print $1 "\t" $2 }
    ' "${snapshot}" | LC_ALL=C sort -u >"${expected}"
    [ -s "${expected}" ] || exit 0

    # Read each member's local bbolt view with a serializable range. A failed
    # read or malformed response leaves the oracle inconclusive: a member that
    # is still recovering is not evidence of data loss.
    conclusive=1
    failed=0
    member=1
    for endpoint in "${member_endpoint_1}" "${member_endpoint_2}" "${member_endpoint_3}"; do
      actual_raw=${actual_raw_prefix}${member}.raw
      actual_unsorted=${actual_raw_prefix}${member}.unsorted
      actual=${actual_raw_prefix}${member}
      if ! ctl_member "${endpoint}" get museum/ --prefix --consistency=s >"${actual_raw}" 2>/dev/null; then
        conclusive=0
      elif awk '
        NR % 2 == 1 { key = $0; next }
        { print key "\t" $0 }
        END { if (NR % 2 != 0) exit 1 }
      ' "${actual_raw}" >"${actual_unsorted}" \
        && LC_ALL=C sort "${actual_unsorted}" >"${actual}"; then
        missing=$(LC_ALL=C comm -23 "${expected}" "${actual}")
        [ -z "${missing}" ] || failed=1
      else
        conclusive=0
      fi
      member=$((member + 1))
    done
    [ "${conclusive}" -eq 1 ] || exit 0
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
