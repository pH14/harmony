#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-6 masked-register-digest lane (bead hm-3bwm) — the named condition on the AA-6
# LinuxGuest PROVISIONAL->full-GO upgrade. Boot the pinned AA-5(c)/AA-6 LinuxGuest under the
# AA-6 INJECTION configuration (the SAME config as the merged PR #139 matrix: `--inject-ppi 22
# --inject-at-work 1 --seed 1`, PPI-22 pending latch at the first exact refresh landing) for
# N same-seed reps, capturing each rep's `masked_regs_digest` (the full register file MINUS
# exactly {x29, SP}) and `injected_landed_digest` (hm-fiqo's injection-Moment register witness).
# `aa6-masked-digest-check.py` then adjudicates bit-identity across all reps.
#
# CPU-pinned per docs/BOX-PINNING.md — the Altra N1 has no SMT, so a single dedicated core
# (default 60, the core the merged AA-6 run used) is the correct pinned posture.
#
# SMOKE-FIRE FIRST (Environment §): run a ~20-rep batch, eyeball the verdict, THEN the >=1000.
#   taskset is applied to each linux-boot; run the whole script detached for the long batch:
#     nohup setsid bash host/aa6-masked-digest-lane.sh <reps> <core> <tag> </dev/null \
#         >~/aa6-masked-<tag>.log 2>&1 &
#
# Usage: aa6-masked-digest-lane.sh <reps> [core] [tag]
#   reps  number of same-seed injected boots (20 for the smoke, >=1000 for the gate)
#   core  isolated core to pin to (default 60; sibling N/A on the no-SMT Altra)
#   tag   evidence subdir tag (default: the rep count)
#
# The pinned artifacts (results/aa-5/live-20260721): Image d0161a7d..., initramfs 604733be...
# Override their PATHS with $IMAGE/$INITRAMFS; the sha256 PINS are fixed and the harness
# verifies each artifact against them, so a stray build cannot masquerade as the gate.
set -uo pipefail

REPS="${1:?usage: aa6-masked-digest-lane.sh <reps> [core] [tag]}"
CORE="${2:-60}"
TAG="${3:-$REPS}"

SPIKE=./target/release/arm-spike
IMAGE="${IMAGE:-$HOME/harmony-linux/Image}"
INITRAMFS="${INITRAMFS:-$HOME/harmony-linux/initramfs.cpio.gz}"
IMAGE_SHA256=d0161a7d41309b6e9139534d99c8c3d24152c0b10c06b4829443402698c5aefe
INITRAMFS_SHA256=604733be3338ac55cc0f387ba55b7b6b31250d158761ca2cc422cf2e37d08573

# The AA-6 injection configuration — fixed, identical across every rep so they form one
# same-seed replay group. PPI 22 is the UNWIRED interrupt (never the clockevent's PPI 20);
# --inject-at-work 1 fires at the first exact refresh landing (work 10,000,000).
INJECT_PPI=22
INJECT_AT_WORK=1
SEED=1
CONDITION=pinned-solo
SKID_MARGIN=1024

OUT="results/aa-6/masked-digest-$TAG"
CONSOLE_TMP="$OUT/.console"
rm -rf -- "$OUT"; mkdir -p "$OUT" "$CONSOLE_TMP"

for tool in "$SPIKE" "$IMAGE" "$INITRAMFS"; do
  [ -e "$tool" ] || { echo "FAIL: missing $tool (build the spike / stage the pinned Image+initramfs)"; exit 2; }
done

# Record the injection config explicitly in the evidence dir (Environment §: the run-set
# attestation hm-oh3v is separate work, but the config must not be left undocumented).
cat >"$OUT/config.json" <<JSON
{
  "lane": "aa6-masked-register-digest",
  "bead": "hm-3bwm",
  "injection": "ON",
  "inject_ppi": $INJECT_PPI,
  "inject_at_work": $INJECT_AT_WORK,
  "injected_at_work_expected": 10000000,
  "seed": $SEED,
  "condition": "$CONDITION",
  "skid_margin": $SKID_MARGIN,
  "core": $CORE,
  "reps": $REPS,
  "image_sha256": "$IMAGE_SHA256",
  "initramfs_sha256": "$INITRAMFS_SHA256",
  "masked_excluded_gprs": "x29:0x603000000010003a,SP:0x603000000010003e",
  "masked_excluded_host_time": "CNTPCT_EL0,CNTPCTSS_EL0,CNTVCTSS_EL0,KVM_REG_ARM_TIMER_CNT"
}
JSON

echo "== AA-6 masked-register-digest lane: $REPS reps, core $CORE, tag $TAG =="
uname -r
fail=0
for i in $(seq 1 "$REPS"); do
  rep=$(printf '%04d' "$i")
  console="$CONSOLE_TMP/console-$rep.bin"
  if ! taskset -c "$CORE" "$SPIKE" linux-boot \
      --image "$IMAGE" --image-sha256 "$IMAGE_SHA256" \
      --initramfs "$INITRAMFS" --initramfs-sha256 "$INITRAMFS_SHA256" \
      --core "$CORE" --skid-margin "$SKID_MARGIN" \
      --inject-ppi "$INJECT_PPI" --inject-at-work "$INJECT_AT_WORK" \
      --seed "$SEED" --condition "$CONDITION" \
      --console-out "$console" </dev/null >"$OUT/rep-$rep.stdout" 2>"$OUT/rep-$rep.err"; then
    echo "FAIL: rep $rep boot returned non-zero"; cat "$OUT/rep-$rep.err"; fail=1; break
  fi
  # Keep only the first rep's console for provenance; the masked digest lives in stdout, and
  # 1000 console.bin would bloat the evidence dir.
  if [ "$i" -eq 1 ]; then mv "$console" "$OUT/console-first.bin"; else rm -f "$console"; fi
  if [ $((i % 100)) -eq 0 ]; then echo "  ... $i/$REPS reps"; fi
done
rmdir "$CONSOLE_TMP" 2>/dev/null || true

if [ "$fail" != 0 ]; then echo "RESULT: FAIL (a rep did not boot cleanly) — see $OUT"; exit 1; fi

echo "== adjudicate: masked digest + injection-Moment witness bit-identical across $REPS reps =="
python3 host/aa6-masked-digest-check.py --run-dir "$OUT" --min-reps "$REPS" --out "$OUT/verdict.json"
rc=$?
echo "verdict: $OUT/verdict.json"
exit $rc
