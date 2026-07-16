#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86 N-0: launch the minimal L1 probe under stock-KVM L0. One run-set.
# Usage: run-l1-probe.sh <runset-name>
set -euo pipefail

BASE=/root/nested-x86-spike/n0
KVER=6.12.90+deb13.1-amd64
RS="$BASE/results/${1:?runset name required}"
mkdir -p "$RS"

QEMU=/usr/bin/qemu-system-x86_64
CPUSET=3   # pinned per box core discipline (leased core set)

# hash-verify the probe image + kernel against the build manifest BEFORE boot
# (PR #98 round-5 P2: recording hashes at launch is not pinning — a post-build
# image swap would otherwise produce valid-looking evidence), and retain the
# manifest with the runset.
MANIFEST="$BASE/build-manifest.json"
[ -f "$MANIFEST" ] || { echo "PIN MANIFEST MISSING: $MANIFEST (run build-l1-probe.sh first)"; exit 1; }
pin_get() { grep -o "\"$1\": \"[0-9a-f]*\"" "$MANIFEST" | head -1 | cut -d'"' -f4; }
pin_verify() { # pin_verify <file> <want> <label>
  local got; got=$(sha256sum "$1" | cut -d' ' -f1)
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $3 ($1): got $got want $2"; exit 1; }
}
WANT_INITRD=$(pin_get "sha256_l1-probe.cpio.gz")
WANT_KERN=$(pin_get "sha256_vmlinuz-$KVER")
[ -n "$WANT_INITRD" ] && [ -n "$WANT_KERN" ] || { echo "PIN MANIFEST INCOMPLETE: $MANIFEST"; exit 1; }
pin_verify "$BASE/l1-probe.cpio.gz" "$WANT_INITRD" probe-initrd
pin_verify "/boot/vmlinuz-$KVER" "$WANT_KERN" l1-kernel
cp "$MANIFEST" "$RS/build-manifest.json"
echo "PIN_VERIFIED initrd=$WANT_INITRD kernel=$WANT_KERN"

{
  echo "{"
  echo "  \"qemu_sha256\": \"$(sha256sum $QEMU | cut -d' ' -f1)\","
  echo "  \"kernel_sha256\": \"$(sha256sum /boot/vmlinuz-$KVER | cut -d' ' -f1)\","
  echo "  \"initrd_sha256\": \"$(sha256sum $BASE/l1-probe.cpio.gz | cut -d' ' -f1)\","
  echo "  \"l0_kvm_intel_nested\": \"$(cat /sys/module/kvm_intel/parameters/nested)\","
  echo "  \"l0_kvm_enable_pmu\": \"$(cat /sys/module/kvm/parameters/enable_pmu)\","
  echo "  \"cpuset\": \"$CPUSET\","
  echo "  \"cmdline\": \"q35,accel=kvm -cpu host,pmu=on -smp 1 -m 2048\","
  echo "  \"started\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/env.json"

rc=0
timeout 180 taskset -c $CPUSET $QEMU \
    -machine q35,accel=kvm \
    -cpu host,pmu=on \
    -smp 1 -m 2048 \
    -kernel /boot/vmlinuz-$KVER \
    -initrd "$BASE/l1-probe.cpio.gz" \
    -append "console=ttyS0 rdinit=/init panic=-1" \
    -display none -monitor none -no-reboot \
    -serial "file:$RS/console.log" \
    </dev/null >"$RS/qemu-stdout.log" 2>&1 || rc=$?

echo "qemu_rc=$rc" >> "$RS/env.json.rc"

# fail-closed verdict (PR #98 round-5 P1): l1-init.sh prints the done marker
# even after module/probe failures (the retained runset-001 demonstrates it:
# 'kvm: FAILED' followed by L1_DONE). Green requires qemu rc=0 AND the run
# completing AND zero FAILED module markers AND /dev/kvm present at L1 AND a
# complete probe sentinel pair.
C="$RS/console.log"
fails=$(grep -c ": FAILED" "$C" || true)
kvm_present=$(grep -c "L1_DEV_KVM_PRESENT" "$C" || true)
pb=$(grep -c "NESTED_X86_PROBE_BEGIN" "$C" || true)
pe=$(grep -c "NESTED_X86_PROBE_END" "$C" || true)
grep -q "NESTED_X86_L1_DONE" "$C" || { echo "RUN_INCOMPLETE $RS (no L1_DONE)"; exit 1; }
# round-6 P1: the probe's own exit status (emitted by l1-init.sh as
# NESTED_X86_PROBE_RC) must be 0, and its output must extract + validate as
# JSON — the sentinels alone never implied the probe ran successfully.
grep -q "NESTED_X86_PROBE_RC rc=0" "$C" || {
  echo "RUN_PROBE_FAILED $RS (probe rc line: $(grep -o 'NESTED_X86_PROBE_RC.*' "$C" || echo absent))"
  exit 1
}
bash /root/nested-x86-spike/extract-probe-json.sh "$C" > "$RS/probe-validated.json" || {
  echo "RUN_PROBE_FAILED $RS (probe output does not validate as JSON)"
  exit 1
}
# round-8 P1: semantically validate the fields the certification BASIS
# requires (docs/NESTED-X86.md §N-0 acceptance) — scoped honestly: N-0 is a
# truth TABLE, so only the required-for-basis capabilities gate the run;
# optional/absent capabilities are data. Required: the trap-closure and MTF
# controls, both PERF_GLOBAL_CTRL load controls, EPT, a usable vPMU
# (arch-perfmon >= 2, >= 1 GP counter), the measured 0x1c4 sniff present,
# and the probe having completed its full sequence. The PMI sniff is
# validated when present (later probe versions emit it).
python3 - "$RS/probe-validated.json" <<'PYEOF' || { echo "RUN_PROBE_FAILED $RS (required-for-basis fields)"; exit 1; }
import json, sys
d = json.load(open(sys.argv[1]))
bad = []
for k in ("ctl_rdtsc_exiting", "ctl_mtf", "ctl_secondary_controls", "ctl2_ept",
          "ctl2_rdrand_exiting", "ctl2_rdseed_exiting",
          "exit_load_perf_global_ctrl", "entry_load_perf_global_ctrl"):
    if d.get(k) is not True:
        bad.append(f"{k}={d.get(k)!r} (required true)")
if not isinstance(d.get("perfmon_version"), int) or d["perfmon_version"] < 2:
    bad.append(f"perfmon_version={d.get('perfmon_version')!r} (required >= 2)")
if not isinstance(d.get("gp_counters"), int) or d["gp_counters"] < 1:
    bad.append(f"gp_counters={d.get('gp_counters')!r} (required >= 1)")
# round-9 P1: probe.c encodes perf failures as VALUES, not absent keys — a
# failed perf_event_open turns the whole sniff into an error STRING, a failed
# read leaves u64::MAX in the count arrays, and pmi reps carry {"error": ...}
# entries. Parse the expected nested shapes and reject every error encoding.
READ_FAIL = 2**64 - 1
sniff = d.get("sniff_raw_0x1c4_br_cond")
if not isinstance(sniff, dict) or not sniff:
    bad.append(f"sniff_raw_0x1c4_br_cond={sniff!r} (must be a non-empty object; "
               "a string is probe.c's perf_event_open-failure encoding)")
else:
    for k, v in sniff.items():
        if (not isinstance(v, list) or not v
                or not all(isinstance(x, int) and 0 <= x < READ_FAIL for x in v)):
            bad.append(f"sniff_raw_0x1c4_br_cond[{k}]={v!r} (non-int/read-failure entry)")
if d.get("probe") != "done":
    bad.append(f"probe={d.get('probe')!r} (sequence incomplete)")
# round-10: the differential assertion is emitted by the current probe —
# required-if-present (retained probes predate it; their 60/60 zero-variance
# arrays already prove the relation, per the audit note).
if "sniff_raw_0x1c4_br_cond_differential" in d \
        and d["sniff_raw_0x1c4_br_cond_differential"] != "exact":
    bad.append(f"sniff differential={d['sniff_raw_0x1c4_br_cond_differential']!r}")
if "pmi_sniff_raw_0x1c4" in d:
    pmi = d["pmi_sniff_raw_0x1c4"]
    if not isinstance(pmi, dict) or not pmi:
        bad.append(f"pmi_sniff_raw_0x1c4={pmi!r} (must be a non-empty object)")
    else:
        # round-10 P1: every rep of every combination must MATCH the armed
        # expectation — ring_samples == signals == expect, zero throttles and
        # other records, and a valid count — not merely be error-free.
        for k, combo in pmi.items():
            reps = combo.get("reps") if isinstance(combo, dict) else None
            expect = combo.get("expect") if isinstance(combo, dict) else None
            if not isinstance(reps, list) or not reps or not isinstance(expect, int):
                bad.append(f"pmi_sniff_raw_0x1c4[{k}] malformed (reps/expect)")
                continue
            for i, r in enumerate(reps):
                if not isinstance(r, dict) or "error" in r:
                    bad.append(f"pmi[{k}] rep {i}: {r!r}")
                elif (r.get("ring_samples") != expect or r.get("signals") != expect
                      or r.get("throttles") != 0 or r.get("other_records") != 0
                      or not isinstance(r.get("count"), int)
                      or not 0 <= r["count"] < READ_FAIL):
                    bad.append(f"pmi[{k}] rep {i} != expect {expect}: {r!r}")
if bad:
    print("PROBE_BASIS_FIELDS_FAILED:", "; ".join(bad))
    sys.exit(1)
print("PROBE_BASIS_FIELDS_OK")
PYEOF
if [ "$rc" -ne 0 ] || [ "$fails" -ne 0 ] || [ "$kvm_present" -lt 1 ] \
   || [ "$pb" -lt 1 ] || [ "$pe" -lt 1 ]; then
  echo "RUN_PROBE_FAILED $RS (qemu_rc=$rc failed_markers=$fails kvm_present=$kvm_present probe=$pb/$pe)"
  grep ": FAILED\|L1_DEV_KVM" "$C" || true
  exit 1
fi
echo "RUN_OK $RS (modules clean, /dev/kvm present, probe rc=0, JSON validated)"
