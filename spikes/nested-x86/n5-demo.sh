#!/bin/bash
# nested-x86 N-5: the one-command demonstration.
#
#   ./spikes/nested-x86/n5-demo.sh /root/nested-x86-n5
#
# From THIS source tree (a fresh checkout/unpack of spike/nested-x86 on the
# box), with stock-KVM nested=1 at L0: builds the gate binaries + C1 payloads,
# assembles the content-pinned consonance appliance image, boots it under
# stock-KVM L0, runs the same-seed determinism gates end-to-end NESTED
# (live_determinism + the 100-rep same-seed repeat gate), and emits an
# evidence bundle. Exits 0 iff every gate passed.
#
# Requirements on the box (all preexisting, hash-verified during the run):
#   /root/kvm-spike/deb612/...      patched 6.12.90 kvm modules
#   /root/harmony-pr44/guest/build  pinned L2 postgres pair
#   /boot/vmlinuz-6.12.90+deb13.1   L1 kernel (= box kernel)
#   rust toolchain, qemu-system-x86_64, busybox (static)
set -euo pipefail

WORK="${1:?work dir (e.g. /root/nested-x86-n5)}"
SRC="$(cd "$(dirname "$0")/../.." && pwd)"
SPIKE="$SRC/spikes/nested-x86"
mkdir -p "$WORK"

echo "=== N5 [1/4] build gates + payloads (source: $SRC)"
cd "$SRC"
( cd guest/payloads && cargo build --release )
BINS=$(cargo test --no-run -p vmm-core --test live_determinism --test n3_repeat_gate \
        --message-format=json 2>/dev/null \
        | grep -o '"executable":"[^"]*"' | cut -d'"' -f4 | sort -u)
echo "$BINS"

echo "=== N5 [2/4] assemble the content-pinned appliance"
# the appliance layout mirrors /root/harmony-nested compile-time paths; a fresh
# tree still records its own identity in the manifest
echo "n5-demo from $SRC" > "$SRC/.spike-source-commit" 2>/dev/null || true
APPLIANCE_BASE="$WORK" SRCROOT="$SRC" INIT_SCRIPT="$SPIKE/appliance/l1-appliance-init.sh" \
  bash "$SPIKE/appliance/build-appliance.sh" $BINS

echo "=== N5 [3/4] boot nested + run the same-seed gates"
rc=0
APPLIANCE_BASE="$WORK" bash "$SPIKE/appliance/run-appliance.sh" "$WORK/results/n5-demo" 3600 \
  "harmony.gates=live_determinism,n3_repeat_gate harmony.env=N3_REPS=100,N3_ITEM=insn-rng" || rc=$?

echo "=== N5 [4/4] verdict + evidence bundle"
C="$WORK/results/n5-demo/console.log"
grep -E "GATE_RC" "$C"
PASS=1
grep -q "NESTED_X86_GATE_RC live_determinism rc=0" "$C" || PASS=0
grep -q "NESTED_X86_GATE_RC n3_repeat_gate rc=0" "$C" || PASS=0
SUMMARY=$(grep -o 'N3JSON {"event":"summary".*' "$C" | tail -1)
echo "$SUMMARY"
{
  echo "{"
  echo "  \"n5_demo\": $([ $PASS = 1 ] && echo '"PASS"' || echo '"FAIL"'),"
  echo "  \"gate_summary\": ${SUMMARY#N3JSON },"
  echo "  \"appliance_manifest\": \"$WORK/build-manifest.json\","
  echo "  \"console\": \"$C\","
  echo "  \"finished\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$WORK/results/n5-demo/verdict.json"
cat "$WORK/results/n5-demo/verdict.json"
[ $PASS = 1 ]
