#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

journal=/tmp/etcd/journal/acked
verified=/tmp/etcd/journal/verified
member_endpoint_1=http://127.0.0.1:2379
member_endpoint_2=http://127.0.0.1:2381
member_endpoint_3=http://127.0.0.1:2383

ctl_member() {
  endpoint=$1
  shift
  ETCDCTL_API=3 /opt/etcd/etcdctl --endpoints="${endpoint}" "$@"
}

# etcdctl prints a range as alternating key and value lines. Exits non-zero on
# a truncated response so a half-read range cannot pass as a member's contents.
pair_range_output() {
  awk '
    NR % 2 == 1 { key = $0; next }
    { print key "\t" $0 }
    END { if (NR % 2 != 0) exit 1 }
  '
}

# Write one member's whole `museum/` prefix as raw alternating lines.
read_member_prefix() {
  ctl_member "$1" get museum/ --prefix --consistency=s
}

# Write only the sequence span each worker contributed to the current window.
# `window_bounds` holds one `worker low high` line per worker. Keys are zero
# padded, so one range request per worker isolates exactly that span. Returns
# non-zero when any of those requests failed.
read_member_window() {
  endpoint=$1
  status=0
  while read -r worker low high; do
    [ -n "${worker}" ] || continue
    from=$(printf 'museum/%s/key-%012d' "${worker}" "${low}")
    # An etcdctl range end is exclusive.
    to=$(printf 'museum/%s/key-%012d' "${worker}" "$((high + 1))")
    ctl_member "${endpoint}" get "${from}" "${to}" --consistency=s || status=1
  done <"${window_bounds}"
  return "${status}"
}

# Compare an expectation file against every member's local bbolt view and emit
# the case's verdict. `reader` names a function called with one endpoint that
# writes that member's raw range output to standard output.
#
# A failed read or a malformed response leaves the oracle inconclusive and
# silent: a member that is still recovering is not evidence of data loss.
# Returns 1 when every member agreed, 2 on a loss, and 0 when inconclusive.
#
# A restarted member reports itself healthy while it is still applying the
# entries it missed, so a single read of a lagging member misses acknowledged
# keys it will hold moments later. The two cases are told apart by whether the
# member converges: applying shrinks the missing set, and a key the member will
# never hold keeps it at the same size. Each member is therefore read until its
# missing set empties or stops shrinking.
settle_plateau_reads=15

compare_against_members() {
  expected=$1
  reader=$2
  work=${expected}.cmp
  conclusive=1
  failed=0
  for endpoint in "${member_endpoint_1}" "${member_endpoint_2}" "${member_endpoint_3}"; do
    remaining=-1
    plateau=0
    while :; do
      if ! "${reader}" "${endpoint}" >"${work}.raw" 2>/dev/null; then
        conclusive=0
        break
      fi
      if ! pair_range_output <"${work}.raw" >"${work}.paired" \
        || ! LC_ALL=C sort "${work}.paired" >"${work}.sorted"; then
        conclusive=0
        break
      fi
      missing=$(LC_ALL=C comm -23 "${expected}" "${work}.sorted" | wc -l)
      if [ "${missing}" -eq 0 ]; then
        break
      fi
      if [ "${remaining}" -lt 0 ] || [ "${missing}" -lt "${remaining}" ]; then
        remaining=${missing}
        plateau=0
      else
        plateau=$((plateau + 1))
        if [ "${plateau}" -ge "${settle_plateau_reads}" ]; then
          failed=1
          break
        fi
      fi
      sleep 0.2
    done
  done
  rm -f "${work}.raw" "${work}.paired" "${work}.sorted"
  if [ "${conclusive}" -ne 1 ]; then
    return 0
  fi
  echo '@reachable 11'
  if [ "${failed}" -eq 0 ]; then
    echo '@always 1 1'
    return 1
  fi
  echo '@always 1 0'
  return 2
}

# Keep only complete, workload-shaped records: workers append concurrently, so
# the journal's final line can be a partial write.
select_entries() {
  awk -F '	' '
    $1 ~ /^museum\/[1-9][0-9]*\/key-[0-9]+$/ &&
    $2 ~ /^value-[1-9][0-9]*-[0-9]+$/ { print $1 "\t" $2 }
  '
}

case "$1" in
  2)
    # The window check: verify what has been acknowledged since the last
    # passing check. The watermark is a byte offset, so the check reads only
    # the bytes appended since then and never the whole journal. A check that
    # re-read the history would cost more on every run and would eventually
    # consume the processor the workload needs. Hook 3 catches what a window
    # stepped over.
    [ -s "${journal}" ] || exit 0
    work=${journal}.$$
    expected=${work}.expected
    window=${work}.window
    window_bounds=${work}.bounds
    trap 'rm -f "${expected}" "${window}" "${window_bounds}"' EXIT

    start=0
    if [ -f "${verified}" ]; then
      start=$(awk 'NR == 1 && $0 ~ /^[0-9]+$/ { print $0 }' "${verified}")
      [ -n "${start}" ] || start=0
    fi
    size=$(wc -c <"${journal}")
    [ "${size}" -gt "${start}" ] || exit 0

    tail -c "+$((start + 1))" "${journal}" | head -c "$((size - start))" >"${window}"
    # Workers append concurrently, so the window can end mid-record. Step its
    # end back to the last newline, so the next window starts on a boundary and
    # no record is skipped.
    consumed=$(LC_ALL=C awk '{ total += length($0) + 1 } END { print total + 0 }' "${window}")
    if [ -n "$(tail -c 1 "${window}")" ]; then
      fragment=$(LC_ALL=C awk 'END { print length($0) + 1 }' "${window}")
      consumed=$((consumed - fragment))
    fi
    [ "${consumed}" -gt 0 ] || exit 0
    head -c "${consumed}" "${window}" | select_entries \
      | LC_ALL=C sort -u >"${expected}"
    [ -s "${expected}" ] || exit 0

    awk -F '	' '
      {
        split($1, path, "/")
        worker = path[2]
        sequence = path[3]
        sub(/^key-/, "", sequence)
        sequence += 0
        if (!(worker in low) || sequence < low[worker]) low[worker] = sequence
        if (!(worker in high) || sequence > high[worker]) high[worker] = sequence
      }
      END {
        for (worker in low) printf "%s %d %d\n", worker, low[worker], high[worker]
      }
    ' "${expected}" >"${window_bounds}"

    set +e
    compare_against_members "${expected}" read_member_window
    verdict=$?
    set -e
    # Advance the watermark only when every member was read and agreed.
    if [ "${verdict}" -eq 1 ]; then
      echo "$((start + consumed))" >"${verified}"
    fi
    exit 0
    ;;
  3)
    # The full sweep: compare the entire journal against every member. Run once
    # at the end of a measurement, so a loss no window covered is still
    # reported.
    [ -s "${journal}" ] || exit 0
    snapshot=${journal}.$$
    expected=${snapshot}.expected
    trap 'rm -f "${snapshot}" "${expected}"' EXIT
    cp "${journal}" "${snapshot}" 2>/dev/null || exit 0
    select_entries <"${snapshot}" | LC_ALL=C sort -u >"${expected}"
    [ -s "${expected}" ] || exit 0

    set +e
    compare_against_members "${expected}" read_member_prefix
    set -e
    exit 0
    ;;
  *)
    echo "unknown hook $1" >&2
    exit 2
    ;;
esac
