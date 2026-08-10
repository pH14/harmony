#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Apply the harmony-arm spike runtime posture (docs/ARM-ALTRA.md §Box discipline,
# record-then-modify). Idempotent; run with sudo AFTER EVERY REBOOT — none of this
# persists. The restore target is spikes/arm-altra/results/box-baseline-manifest.json
# (perf_event_paranoid default, dmesg_restrict=1, governor ondemand on 0-79).
#
#   sudo ./spike-posture.sh        # apply
#   sudo ./spike-posture.sh restore  # return to the recorded baseline posture
set -euo pipefail

restore="${1:-}"

if [[ "$restore" == "restore" ]]; then
  sysctl -w kernel.perf_event_paranoid=4
  sysctl -w kernel.dmesg_restrict=1
  for g in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
    echo ondemand > "$g"
  done
  echo "POSTURE: baseline restored (paranoid=4, dmesg_restrict=1, ondemand 0-79)"
  exit 0
fi

# Unprivileged perf_event_open of the raw pinned BR_RETIRED event (AA-0 row
# perf-raw-0x21-pinned; provisioning set this 2026-07-17).
sysctl -w kernel.perf_event_paranoid=-1

# Unprivileged klogctl: the effective-KVM-mode read (sys::kvm_mode's kernel-log
# fallback) — this kernel exposes no /sys/module/kvm_arm/parameters/mode.
sysctl -w kernel.dmesg_restrict=0

# Fixed-frequency posture for every core the spike measures on (wall-clock hygiene;
# V-time counts are frequency-independent). All 80 cores: the box is exclusively
# the spike's, and builds benefit too.
for g in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
  echo performance > "$g"
done

echo "POSTURE: applied (paranoid=-1, dmesg_restrict=0, performance 0-79)"
