#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# capture-baseline.sh — AE-0 record-then-modify baseline capture (docs/AMD-EPYC.md
# §Box discipline). Runs ON the box, emits one stable-JSON manifest on stdout
# (sorted keys, machine-written — never handwritten from terminal output).
#
# The first run against the pristine box IS the restore target
# (results/box-baseline-manifest.json). Re-run at lock-yield / spike-end and
# diff the restorable subset (`--restore-view`) to verify restoration.
#
# Usage:  capture-baseline.sh > manifest.json
#         capture-baseline.sh --restore-view < manifest.json   # project restorable subset
set -euo pipefail

if [[ "${1:-}" == "--restore-view" ]]; then
  # Project the subset of a manifest that restoration must reproduce.
  # (Excludes: timestamps, uptime, boot-to-boot volatile fields.)
  exec python3 -c '
import json,sys
m=json.load(sys.stdin)
keep={k:m[k] for k in ["cpu_identity","event_encoding","kvm","ls_cfg_per_cpu","nmi_watchdog","perf_event_paranoid","scaling_governor_per_cpu","smt","online_cpus"] if k in m}
print(json.dumps(keep,indent=2,sort_keys=True))
'
fi

msr_read() { # msr_read <cpu> <msr-hex>  -> 16-hex-digit value (or "unreadable")
  local cpu="$1" msr="$2"
  sudo dd if=/dev/cpu/"$cpu"/msr bs=8 skip=$(($msr)) iflag=skip_bytes count=1 2>/dev/null \
    | od -A n -t x8 | tr -d ' \n' || echo unreadable
}

# --- gather raw facts into a temp dir, then let python assemble stable JSON ---
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

uname -r  > "$T/kernel_release"
uname -v  > "$T/kernel_version"
cat /proc/cmdline > "$T/cmdline"
# No box identifiers in committed artifacts (tasks/123 §Environment): the box is
# reached via the AMD_BOX_SSH alias convention (docs/BOX-PINNING.md); its hostname
# is not recorded.
echo "redacted (AMD_BOX_SSH alias convention, docs/BOX-PINNING.md)" > "$T/hostname"
date -u +%Y-%m-%dT%H:%M:%SZ > "$T/captured_at"

# CPU identity (first processor stanza is representative; identity is per-package)
awk -F': ' '
  /^processor/ {n+=1} n>1 {exit}
  /^vendor_id/ {print "vendor_id="$2}
  /^cpu family/ {print "family="$2}
  /^model\t/ {print "model="$2}
  /^model name/ {print "model_name="$2}
  /^stepping/ {print "stepping="$2}
  /^microcode/ {print "microcode="$2}
' /proc/cpuinfo > "$T/cpu_identity"

# topology + SMT
for c in /sys/devices/system/cpu/cpu[0-9]*; do
  n=${c##*/cpu}
  echo "$n:$(cat "$c"/topology/core_id):$(cat "$c"/topology/thread_siblings_list)"
done | sort -t: -k1,1n > "$T/topology"
cat /sys/devices/system/cpu/smt/control > "$T/smt_control"
cat /sys/devices/system/cpu/smt/active  > "$T/smt_active"
cat /sys/devices/system/cpu/online      > "$T/online_cpus"

# governors (per online cpu)
for c in /sys/devices/system/cpu/cpu[0-9]*; do
  n=${c##*/cpu}
  g=$(cat "$c"/cpufreq/scaling_governor 2>/dev/null || echo none)
  echo "$n:$g"
done | sort -t: -k1,1n > "$T/governors"
cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null > "$T/boost" || echo unknown > "$T/boost"

# KVM identity: module files hashed, params captured
modinfo -F filename kvm_amd > "$T/kvm_amd_path"
modinfo -F srcversion kvm_amd > "$T/kvm_amd_srcversion"
sha256sum "$(cat "$T/kvm_amd_path")" | awk '{print $1}' > "$T/kvm_amd_sha256"
modinfo -F filename kvm > "$T/kvm_path"
modinfo -F srcversion kvm > "$T/kvm_srcversion"
sha256sum "$(cat "$T/kvm_path")" | awk '{print $1}' > "$T/kvm_sha256"
grep -r . /sys/module/kvm_amd/parameters/ | sed 's|.*/parameters/||' | sort > "$T/kvm_amd_params"
grep -r . /sys/module/kvm/parameters/ | sed 's|.*/parameters/||' | sort > "$T/kvm_params"
ls -la /dev/kvm >/dev/null 2>&1 && echo present > "$T/dev_kvm" || echo absent > "$T/dev_kvm"

# MSRs: LS_CFG (0xC0011020, SpecLockMap; rr workaround = bit 54) per cpu; HWCR
for c in /sys/devices/system/cpu/cpu[0-9]*; do
  n=${c##*/cpu}
  echo "$n:0x$(msr_read "$n" 0xC0011020)"
done | sort -t: -k1,1n > "$T/ls_cfg"
echo "0x$(msr_read 0 0xC0010015)" > "$T/hwcr"

# perf posture + the per-generation event encoding as the kernel pins it
cat /proc/sys/kernel/perf_event_paranoid > "$T/perf_event_paranoid"
cat /proc/sys/kernel/nmi_watchdog        > "$T/nmi_watchdog"
perf --version | awk '{print $3}'        > "$T/perf_version"
# the perf JSON tables' encoding for this part (authoritative userspace pin;
# cross-checked against PPR 17h-71h PMCx0C4 and validated live by the AE-0 probe)
perf list --details 2>/dev/null | grep -A2 '^  ex_ret_brn_tkn$' \
  | grep -o 'event=0x[0-9a-f]*' | head -1 > "$T/ex_ret_brn_tkn_encoding" || true

# services running (touched-services accounting)
systemctl list-units --state=running --no-pager --no-legend 2>/dev/null \
  | awk '{print $1}' | sort > "$T/services_running"

python3 - "$T" <<'PY'
import json, os, sys
t = sys.argv[1]
def slurp(n):
    with open(os.path.join(t, n)) as f: return f.read().strip()
def lines(n):
    with open(os.path.join(t, n)) as f: return [l.strip() for l in f if l.strip()]

ident = dict(l.split('=', 1) for l in lines('cpu_identity'))
fam, mod = int(ident['family']), int(ident['model'])
# Zen-generation pin: family 0x17 models 0x30-0x7f are Zen 2 (0x71 = Matisse).
if fam == 23 and 0x30 <= mod <= 0x7f: zen = 'zen2'
elif fam == 23: zen = 'zen1-or-zen+'
elif fam == 25: zen = 'zen3-or-zen4'
elif fam == 26: zen = 'zen5'
else: zen = 'unknown'

manifest = {
  'schema': 'amd-epyc-box-baseline-v1',
  'captured_at': slurp('captured_at'),
  'hostname': slurp('hostname'),
  'hardware_flag': 'Zen 2 core (Ryzen 3600) — NOT an EPYC; platform-scoped facts PROVISIONAL (tasks/123 hardware flag)',
  'cpu_identity': {
    'vendor_id': ident['vendor_id'],
    'family': fam, 'model': mod, 'stepping': int(ident['stepping']),
    'model_name': ident['model_name'],
    'microcode': ident['microcode'],
    'zen_generation': zen,
    'zen_generation_basis': f'family {fam:#x} model {mod:#x}',
  },
  'kernel': {
    'release': slurp('kernel_release'),
    'version': slurp('kernel_version'),
    'cmdline': slurp('cmdline'),
  },
  'kvm': {
    'dev_kvm': slurp('dev_kvm'),
    'kvm_amd': {
      'path': slurp('kvm_amd_path'), 'srcversion': slurp('kvm_amd_srcversion'),
      'sha256': slurp('kvm_amd_sha256'), 'identity': 'stock-signed-distro-module',
      'params': dict(l.split(':', 1) for l in lines('kvm_amd_params')),
    },
    'kvm': {
      'path': slurp('kvm_path'), 'srcversion': slurp('kvm_srcversion'),
      'sha256': slurp('kvm_sha256'),
      'params': dict(l.split(':', 1) for l in lines('kvm_params')),
    },
    'avic_posture': dict(l.split(':', 1) for l in lines('kvm_amd_params')).get('avic', 'unknown'),
    'nested': dict(l.split(':', 1) for l in lines('kvm_amd_params')).get('nested', 'unknown'),
  },
  'topology': [
    {'cpu': int(c), 'core_id': int(k), 'thread_siblings': s}
    for c, k, s in (l.split(':', 2) for l in lines('topology'))
  ],
  'smt': {'control': slurp('smt_control'), 'active': slurp('smt_active')},
  'online_cpus': slurp('online_cpus'),
  'scaling_governor_per_cpu': dict((int(a), b) for a, b in (l.split(':', 1) for l in lines('governors'))),
  'cpufreq_boost': slurp('boost'),
  'ls_cfg_per_cpu': dict((int(a), b) for a, b in (l.split(':', 1) for l in lines('ls_cfg'))),
  'ls_cfg_msr': '0xC0011020',
  'ls_cfg_bit54_speclockmap_workaround': 'clear-on-baseline (speculative lock mapping ENABLED — overcount hazard live; AE-1(c) probes then sets)',
  'hwcr': slurp('hwcr'),
  'perf_event_paranoid': slurp('perf_event_paranoid'),
  'nmi_watchdog': slurp('nmi_watchdog'),
  'perf_version': slurp('perf_version'),
  'event_encoding': {
    'ex_ret_brn_tkn': slurp('ex_ret_brn_tkn_encoding') or 'not-listed',
    'source': 'perf list --details (kernel perf JSON tables for this part)',
  },
  'services_running': lines('services_running'),
}
print(json.dumps(manifest, indent=2, sort_keys=True))
PY
