#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# posture.sh — per-run measurement posture: apply, ATTEST, restore
# (docs/AMD-EPYC.md §Box discipline, §Evidence integrity #4 mechanism attestation).
#
# Pinning + the LS_CFG SpecLockMap workaround are CORRECTNESS conditions, not hygiene
# (doc §2). Every measurement run brackets its payload with:
#   posture.sh apply  --core N [--speclockmap on|off]   > posture.attest.json
#   ... run amd-hammer pinned to N ...
#   posture.sh restore --core N
# The attest JSON is retained beside the run's records; the disposition checker can
# assert the posture the run CLAIMS was actually in force (patched-vs-stock analogue
# for the host-side stage).
#
# The ONE sanctioned deviation is AE-1(c)'s bounded `--speclockmap off` probe, which
# deliberately reproduces the overcount, then the workaround is re-applied permanently.
#
# LS_CFG = MSR 0xC0011020, SpecLockMap workaround = bit 54 (per the baseline manifest
# and rr's Zen workaround). DIRECTION IS MEASURED, NOT ASSUMED: AE-1(c) shows which
# setting eliminates the overcount; this script only sets/clears the bit and records it.
set -euo pipefail

LS_CFG=0xC0011020
BIT54_MASK=$((1 << 54))
BASELINE="${BASELINE:-$HOME/amd-epyc-spike/box-baseline-manifest.json}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing tool: $1" >&2; exit 3; }; }
need rdmsr; need wrmsr
sudo modprobe msr 2>/dev/null || true

rd() { sudo rdmsr -p "$1" -0 "$LS_CFG" 2>/dev/null; }   # 0x-prefixed hex
sib_of() { cat "/sys/devices/system/cpu/cpu$1/topology/thread_siblings_list"; }
# return the sibling cpu id that is not $1 (handles "a,b" or "a-b")
other_sib() {
  local list; list=$(sib_of "$1"); list=${list//-/,}
  local IFS=,; for c in $list; do [ "$c" != "$1" ] && { echo "$c"; return; }; done
}

apply() {
  local core="$1" slm="$2"
  local sib; sib=$(other_sib "$core" || true)
  # 1) idle/offline the SMT sibling (doc §Box discipline SMT caveat) — Zen is SMT-2
  local sib_state="none"
  if [ -n "${sib:-}" ] && [ -w "/sys/devices/system/cpu/cpu$sib/online" ]; then
    echo 0 | sudo tee "/sys/devices/system/cpu/cpu$sib/online" >/dev/null
    sib_state="offlined"
  elif [ -n "${sib:-}" ]; then
    sib_state="present-not-offlineable"
  fi
  # 2) governor -> performance on the measurement core (frequency hygiene; counts are
  #    frequency-independent, but wall-clock skid numbers want a stable clock)
  local gov_prev="unknown"
  if [ -w "/sys/devices/system/cpu/cpu$core/cpufreq/scaling_governor" ]; then
    gov_prev=$(cat "/sys/devices/system/cpu/cpu$core/cpufreq/scaling_governor")
    echo performance | sudo tee "/sys/devices/system/cpu/cpu$core/cpufreq/scaling_governor" >/dev/null 2>&1 || true
  fi
  # 3) LS_CFG bit 54 on BOTH threads of the physical core (LS_CFG is core-scoped on Zen)
  local before after targets=("$core")
  [ -n "${sib:-}" ] && targets+=("$sib")
  before=$(rd "$core")
  for c in "${targets[@]}"; do
    local v newv
    v=$(rd "$c"); v=$((v))
    if [ "$slm" = "on" ]; then newv=$(( v | BIT54_MASK )); else newv=$(( v & ~BIT54_MASK )); fi
    sudo wrmsr -p "$c" "$LS_CFG" "$newv"
  done
  after=$(rd "$core")
  # 4) attest — emit a stable JSON record of the posture actually in force
  local bit54; bit54=$(( ( $(printf '%d' "$after") >> 54 ) & 1 ))
  python3 - "$core" "${sib:-none}" "$sib_state" "$gov_prev" "$before" "$after" "$bit54" "$slm" <<'PY'
import json,sys
core,sib,sibst,gov,before,after,bit54,slm=sys.argv[1:9]
print(json.dumps({
 "schema":"amd-epyc-posture-v1","core":int(core),
 "sibling":sib,"sibling_state":sibst,"governor_prev":gov,
 "ls_cfg_before":before,"ls_cfg_after":after,
 "ls_cfg_bit54":int(bit54),"speclockmap_workaround_requested":slm,
 "ls_cfg_msr":"0xC0011020"
},sort_keys=True))
PY
}

restore() {
  local core="$1"
  local sib; sib=$(other_sib "$core" || true)
  # re-online the sibling
  [ -n "${sib:-}" ] && [ -w "/sys/devices/system/cpu/cpu$sib/online" ] && \
    echo 1 | sudo tee "/sys/devices/system/cpu/cpu$sib/online" >/dev/null || true
  # restore LS_CFG on both threads to the per-cpu baseline value from the manifest
  if [ -f "$BASELINE" ]; then
    for c in "$core" ${sib:-}; do
      local bv
      bv=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["ls_cfg_per_cpu"].get(str(sys.argv[2]),""))' "$BASELINE" "$c" || true)
      [ -n "$bv" ] && sudo wrmsr -p "$c" "$LS_CFG" "$bv" || true
      # restore governor
      local g
      g=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["scaling_governor_per_cpu"].get(str(sys.argv[2]),""))' "$BASELINE" "$c" || true)
      [ -n "$g" ] && [ -w "/sys/devices/system/cpu/cpu$c/cpufreq/scaling_governor" ] && \
        echo "$g" | sudo tee "/sys/devices/system/cpu/cpu$c/cpufreq/scaling_governor" >/dev/null 2>&1 || true
    done
  fi
  echo "restored core $core (+sibling ${sib:-none}) to baseline LS_CFG/governor/online" >&2
}

case "${1:-}" in
  apply)
    shift; core=""; slm="on"
    while [ $# -gt 0 ]; do case "$1" in
      --core) core="$2"; shift 2;; --speclockmap) slm="$2"; shift 2;; *) echo "bad arg $1" >&2; exit 2;; esac; done
    [ -n "$core" ] || { echo "apply needs --core N" >&2; exit 2; }
    apply "$core" "$slm" ;;
  restore)
    shift; core=""
    while [ $# -gt 0 ]; do case "$1" in --core) core="$2"; shift 2;; *) echo "bad arg $1" >&2; exit 2;; esac; done
    [ -n "$core" ] || { echo "restore needs --core N" >&2; exit 2; }
    restore "$core" ;;
  *) echo "usage: posture.sh apply --core N [--speclockmap on|off] | restore --core N" >&2; exit 2;;
esac
