#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
report=$1
shift
mkdir -p "$report"
report=$(cd "$report" && pwd)
privileged=()
if [[ $EUID -ne 0 ]]; then
  privileged=(sudo)
fi
trace_root=/sys/kernel/tracing
if [[ ! -d "$trace_root/events" ]]; then
  "${privileged[@]}" mount -t tracefs tracefs "$trace_root"
fi
instance="$trace_root/instances/harmony-xsave-$$"
"${privileged[@]}" mkdir "$instance"
finish() {
  result=$?
  set +e
  printf '0\n' | "${privileged[@]}" tee "$instance/tracing_on" >/dev/null
  "${privileged[@]}" cat "$instance/trace" > "$report/kvm-exits.txt"
  printf 'test_status=%s\n' "$result" > "$report/status.txt"
  "${privileged[@]}" rmdir "$instance"
  exit "$result"
}
trap finish EXIT
"${privileged[@]}" cat "$instance/events/kvm/kvm_exit/format" > "$report/event-format.txt"
printf '4096\n' | "${privileged[@]}" tee "$instance/buffer_size_kb" >/dev/null
"${privileged[@]}" chmod a+w "$instance/trace_marker"
printf '1\n' | "${privileged[@]}" tee "$instance/events/kvm/kvm_exit/enable" >/dev/null
printf '1\n' | "${privileged[@]}" tee "$instance/tracing_on" >/dev/null
export XSAVE_TRACE_MARKER="$instance/trace_marker"
"$@"
