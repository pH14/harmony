#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

usage="usage: run.sh VERSION COUNT [jepsen test options...] | run.sh down"
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
tool=${JEPSEN_TOOL:-$(command -v podman >/dev/null && echo podman || echo docker)}
project=${JEPSEN_PROJECT:-harmony-jepsen-nats}
cpus=${JEPSEN_CPUSET:-0-5}
nodes=(n1 n2 n3)

if [ "${1:-}" = down ]; then
  for name in control "${nodes[@]}"; do
    "$tool" rm -f -t 0 "$project-$name" >/dev/null 2>&1 || true
  done
  "$tool" network rm "$project" >/dev/null 2>&1 || true
  exit 0
fi

version=${1:?$usage}
count=${2:?$usage}
shift 2
: "${JEPSEN_STORE:?set JEPSEN_STORE to a directory outside the repository}"
mkdir -p "$JEPSEN_STORE"

format=()
netopts=()
[ "$tool" != podman ] || { format=(--format docker); netopts=(--disable-dns); }
"$tool" build "${format[@]}" -q -f "$here/node.Dockerfile" -t harmony-jepsen-nats-node:local "$root" >/dev/null
"$tool" build "${format[@]}" -q -f "$here/control.Dockerfile" -t harmony-jepsen-nats-control:local "$root" >/dev/null

"$tool" network inspect "$project" >/dev/null 2>&1 || "$tool" network create "${netopts[@]}" "$project" >/dev/null
tmpfs=()
[ -z "${JEPSEN_TMPFS:-}" ] || tmpfs=(--tmpfs /opt:exec,mode=755,size=1g)
for node in "${nodes[@]}"; do
  "$tool" inspect "$project-$node" >/dev/null 2>&1 || "$tool" run -d \
    --name "$project-$node" --hostname "$node" --network "$project" --network-alias "$node" \
    --cap-add NET_ADMIN --cap-add SYS_ADMIN --device /dev/fuse --security-opt apparmor=unconfined \
    --cpuset-cpus "$cpus" --memory 2g "${tmpfs[@]}" harmony-jepsen-nats-node:local >/dev/null
done
"$tool" inspect "$project-control" >/dev/null 2>&1 || "$tool" run -d \
  --name "$project-control" --hostname control --network "$project" \
  --cpuset-cpus "$cpus" --memory 5g -v "$JEPSEN_STORE:/jepsen/nats/store" \
  harmony-jepsen-nats-control:local >/dev/null

hosts=""
for node in "${nodes[@]}"; do
  ip=$("$tool" inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$project-$node")
  hosts+="$ip $node"$'\n'
done
for name in control "${nodes[@]}"; do
  "$tool" exec -i "$project-$name" sh -c 'grep -q " n1$" /etc/hosts || cat >>/etc/hosts' <<<"$hosts"
done

key=$("$tool" exec "$project-control" cat /root/.ssh/id_ed25519.pub)
for node in "${nodes[@]}"; do
  "$tool" exec -i "$project-$node" sh -c \
    'mkdir -p -m 700 /root/.ssh && cat >/root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys' <<<"$key"
done

runs="$JEPSEN_STORE/runs.tsv"
[ -f "$runs" ] || printf 'project\tversion\tstart\tend\texit\toptions\n' >"$runs"
for _ in $(seq "$count"); do
  start=$(date +%s)
  status=0
  "$tool" exec "$project-control" \
    java "-Xmx${JEPSEN_HEAP:-3g}" -Djava.awt.headless=true -jar /jepsen/jepsen-nats.jar test \
    --nodes n1,n2,n3 --username root \
    --version "$version" "$@" || status=$?
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$project" "$version" "$start" "$(date +%s)" "$status" "$*" >>"$runs"
done
