#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Measures how often the cluster reaches a lost acknowledged key under plain
# random member kills, with no Dissonance involved. The image's instrumented
# server needs /dev/harmony, which only exists inside a Harmony guest, so run
# this against an image whose /usr/lib/libvoidstar.so has been removed.
#
# Run by hand:  reachability.sh <iterations> [log path]
# Exits 1 on the first lost key, 0 when the iteration budget runs out.
set -eu

iterations=${1:?missing iteration count}
log=${2:-/tmp/etcd/reachability.log}

harmony=/opt/harmony
journal=/tmp/etcd/journal/acked
member_pid_1=
member_pid_2=
member_pid_3=

random_u16() {
  od -An -N2 -tu2 </dev/urandom | tr -d ' \n'
}

start_member() {
  case $1 in
    1)
      "${harmony}/node.sh" member-1 /tmp/etcd/data/member-1 2379 2380 &
      member_pid_1=$!
      ;;
    2)
      "${harmony}/node.sh" member-2 /tmp/etcd/data/member-2 2381 2382 &
      member_pid_2=$!
      ;;
    3)
      "${harmony}/node.sh" member-3 /tmp/etcd/data/member-3 2383 2384 &
      member_pid_3=$!
      ;;
  esac
}

member_pid() {
  case $1 in
    1) echo "${member_pid_1}" ;;
    2) echo "${member_pid_2}" ;;
    3) echo "${member_pid_3}" ;;
  esac
}

# A restarted member replays its own WAL before it serves, so readiness can
# take much longer than the interval between kills.
await_ready() {
  waited=0
  while [ "${waited}" -lt 600 ]; do
    if "${harmony}/ready.sh" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.2
    waited=$((waited + 1))
  done
  return 1
}

"${harmony}/setup.sh"
start_member 1
start_member 2
start_member 3
if ! await_ready; then
  echo "cluster never became ready" >&2
  exit 2
fi
setsid -f "${harmony}/etcd-writer" >/tmp/etcd/writer.log 2>&1

: >"${log}"
started=$(date +%s)
iteration=0
while [ "${iteration}" -lt "${iterations}" ]; do
  iteration=$((iteration + 1))

  hold_ms=$((100 + $(random_u16) * 2901 / 65536))
  sleep "$((hold_ms / 1000)).$(printf '%03d' "$((hold_ms % 1000))")"

  member=$(($(random_u16) % 3 + 1))
  pid=$(member_pid "${member}")
  kill -9 "${pid}" 2>/dev/null || true
  wait "${pid}" 2>/dev/null || true
  start_member "${member}"

  if ! await_ready; then
    verdict='not-ready'
  else
    verdict=$("${harmony}/hooks.sh" 2 | tr '\n' ' ')
  fi

  acked=$(wc -l <"${journal}" 2>/dev/null || echo 0)
  elapsed=$(($(date +%s) - started))
  printf '%d\t%d\t%d\t%d\t%s\n' \
    "${iteration}" "${elapsed}" "${hold_ms}" "${acked}" "${verdict}" >>"${log}"

  case "${verdict}" in
    *'@always 1 0'*)
      echo "lost acknowledged key at iteration ${iteration} after ${elapsed}s" >&2
      exit 1
      ;;
  esac
done
exit 0
