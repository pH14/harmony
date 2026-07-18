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
# assert the posture the run CLAIMS was actually in force.
#
# The ONE sanctioned deviation is AE-1(c)'s bounded `--speclockmap off` probe, which
# deliberately reproduces the overcount, then the workaround is re-applied permanently.
#
# LS_CFG = MSR 0xC0011020, SpecLockMap workaround = bit 54 (per the baseline manifest
# and rr's Zen workaround). DIRECTION IS MEASURED at AE-1(c), not assumed: this script
# only sets/clears the bit and records the value actually in force.
#
# rdmsr note: `rdmsr` prints BARE hex with leading zeros stripped (e.g. "6404000000000"),
# so every read is parsed as hex explicitly (0x$raw / 16#) — never fed raw to $(( )).
set -euo pipefail

LS_CFG=0xC0011020
BIT54_MASK=$((1 << 54))
# Anchor to the script's own dir, NOT $HOME: under `sudo` $HOME becomes /root and the
# manifest (needed for sibling discovery + baseline restore) would not be found. The
# committed manifest lives under results/ — point there so a FRESH CHECKOUT resolves it.
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BASELINE="${BASELINE:-$SCRIPT_DIR/../results/box-baseline-manifest.json}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing tool: $1" >&2; exit 3; }; }
need rdmsr; need wrmsr
sudo modprobe msr 2>/dev/null || true

# read LS_CFG on a cpu -> normalized 0x-prefixed 16-digit hex (parse-safe)
rd() { printf '0x%016x' "$(( 0x$(sudo rdmsr -p "$1" 0xC0011020 2>/dev/null || echo 0) ))"; }
sib_of() { cat "/sys/devices/system/cpu/cpu$1/topology/thread_siblings_list" 2>/dev/null; }
# sibling cpu id != $1 from the BASELINE topology (robust when the live sibling is
# already offline — its live topology mask drops it, the baseline record does not).
baseline_sib() {
  python3 -c '
import json,sys
m=json.load(open(sys.argv[1])); core=int(sys.argv[2])
for row in m["topology"]:
    if row["cpu"]==core:
        for c in str(row["thread_siblings"]).replace("-",",").split(","):
            if c and int(c)!=core: print(int(c)); sys.exit(0)
' "$BASELINE" "$1" 2>/dev/null || true
}

apply() {
  local core="$1" slm="$2"
  # Fail closed (P1-4): the measurement posture is a CORRECTNESS condition (doc §2). If
  # the baseline manifest is absent we cannot discover the SMT sibling to idle, so refuse
  # rather than silently measure under a live sibling while the attest records "none".
  [ -f "$BASELINE" ] || { echo "baseline manifest not found at $BASELINE — cannot establish measurement posture" >&2; exit 3; }
  local sib; sib=$(baseline_sib "$core")
  [ -n "${sib:-}" ] || { echo "no SMT sibling for core $core in $BASELINE topology — refusing (SMT confound uncontrolled)" >&2; exit 3; }
  # 1) governor -> performance on the measurement core (frequency hygiene; counts are
  #    frequency-independent, but wall-clock skid numbers want a stable clock)
  local gov_prev="unknown"
  if [ -f "/sys/devices/system/cpu/cpu$core/cpufreq/scaling_governor" ]; then
    gov_prev=$(cat "/sys/devices/system/cpu/cpu$core/cpufreq/scaling_governor")
    echo performance | sudo tee "/sys/devices/system/cpu/cpu$core/cpufreq/scaling_governor" >/dev/null 2>&1 || true
  fi
  # 2) LS_CFG bit 54 on BOTH threads of the physical core, WHILE BOTH ARE ONLINE
  #    (offlining a cpu removes its /dev/cpu/N/msr). Read-modify-write preserving the
  #    low bits; parse as hex explicitly.
  local before after targets=("$core")
  [ -n "${sib:-}" ] && targets+=("$sib")
  before=$(rd "$core")
  local c v newv
  for c in "${targets[@]}"; do
    v=$(( $(rd "$c") ))
    if [ "$slm" = "on" ]; then newv=$(( v | BIT54_MASK )); else newv=$(( v & ~BIT54_MASK )); fi
    sudo wrmsr -p "$c" "$LS_CFG" "$(printf '0x%x' "$newv")"
  done
  after=$(rd "$core")
  # 3) NOW idle/offline the SMT sibling (doc §Box discipline SMT caveat) — Zen is SMT-2
  local sib_state="none"
  if [ -n "${sib:-}" ] && [ -f "/sys/devices/system/cpu/cpu$sib/online" ]; then
    echo 0 | sudo tee "/sys/devices/system/cpu/cpu$sib/online" >/dev/null
    sib_state="offlined"
  elif [ -n "${sib:-}" ]; then
    sib_state="present-not-offlineable"
  fi
  # 4) attest — emit a stable JSON record of the posture actually in force
  local bit54=$(( ( $(( after )) >> 54 ) & 1 ))
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

# restore to the recorded baseline: re-online EVERY offline cpu, then restore LS_CFG +
# governor on every cpu from the manifest (robust "return the box to a recorded state").
restore() {
  local m="$BASELINE"
  local c
  for c in /sys/devices/system/cpu/cpu[0-9]*; do
    local n=${c##*/cpu}
    [ -f "$c/online" ] && [ "$(cat "$c/online")" = 0 ] && echo 1 | sudo tee "$c/online" >/dev/null || true
  done
  if [ -f "$m" ]; then
    for c in /sys/devices/system/cpu/cpu[0-9]*; do
      local n=${c##*/cpu} bv g
      bv=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["ls_cfg_per_cpu"].get(str(sys.argv[2]),""))' "$m" "$n" || true)
      [ -n "$bv" ] && sudo wrmsr -p "$n" "$LS_CFG" "$bv" || true
      g=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["scaling_governor_per_cpu"].get(str(sys.argv[2]),""))' "$m" "$n" || true)
      [ -n "$g" ] && [ -f "$c/cpufreq/scaling_governor" ] && echo "$g" | sudo tee "$c/cpufreq/scaling_governor" >/dev/null 2>&1 || true
    done
  fi
  echo "restored to baseline: all cpus online, LS_CFG + governor reset from $m" >&2
}

case "${1:-}" in
  apply)
    shift; core=""; slm="on"
    while [ $# -gt 0 ]; do case "$1" in
      --core) core="$2"; shift 2;; --speclockmap) slm="$2"; shift 2;; *) echo "bad arg $1" >&2; exit 2;; esac; done
    [ -n "$core" ] || { echo "apply needs --core N" >&2; exit 2; }
    apply "$core" "$slm" ;;
  restore)
    shift  # restore ignores --core: it returns the WHOLE box to baseline
    restore ;;
  *) echo "usage: posture.sh apply --core N [--speclockmap on|off] | restore" >&2; exit 2;;
esac
