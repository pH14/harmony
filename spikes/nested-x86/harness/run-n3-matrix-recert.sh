#!/bin/bash
# nested-x86 re-certification: the N-3 nested matrix at the BINDING floors
# (bead hm-jpu): >=1,000 same-seed repetitions bit-identical PER condition
# (solo / co-tenant other-core / co-tenant same-core / vCPU migration /
# pause-resume at the recorded cadence), fail-closed live-migration, plus a
# 10k nested hammer control at a dedicated seed for the cross-substrate
# final_work equality check against the metal session.
#
# Serialized except the two pause modes, which co-run on disjoint pinned
# cores (3 and 5, no stress generators — task-69 M2: co-running is itself a
# determinism stress test; divergence is a P0 finding, never serialized to
# hide). Stops on first failure — diagnose before spending more box time.
set -uo pipefail
LOG=/root/nested-x86-recert/n3-matrix.log
STATE=/root/nested-x86-recert/n3-matrix.state
RA=/root/nested-x86-spike/n1/src/run-appliance.sh
step() { echo "N3MATRIX $1 $(date -u +%FT%TZ)" | tee -a "$LOG"; echo "running $1" > "$STATE"; }
fail() { echo "N3MATRIX FAILED $1 $(date -u +%FT%TZ)" | tee -a "$LOG"; echo "FAILED $1" > "$STATE"; exit 1; }

# smoke-fire the repeat-gate config once before the floor spends
step smoke
bash "$RA" /root/nested-x86-spike/n3/results/smoke-recert-001 1800 \
  "harmony.gates=n3_repeat_gate harmony.env=N3_REPS=10,N3_ITEM=insn-rng" >> "$LOG" 2>&1 || fail smoke

step solo
bash "$RA" /root/nested-x86-spike/n3/results/solo-recert-001 28800 \
  "harmony.gates=n3_repeat_gate harmony.env=N3_REPS=1000,N3_ITEM=insn-rng" >> "$LOG" 2>&1 || fail solo

step othercore
bash /root/nested-x86-spike/run-n3-stress.sh othercore 1000 othercore-recert-001 >> "$LOG" 2>&1 || fail othercore

step samecore
bash /root/nested-x86-spike/run-n3-stress.sh samecore 1000 samecore-recert-001 >> "$LOG" 2>&1 || fail samecore

step migrate
bash /root/nested-x86-spike/run-n3-stress.sh migrate 1000 migrate-recert-001 >> "$LOG" 2>&1 || fail migrate

step pause-pair
bash /root/nested-x86-spike/run-n3-pause.sh pause-sigstop-recert-001 1000 sigstop >> "$LOG" 2>&1 & P1=$!
CPUSET_OVERRIDE=5 bash /root/nested-x86-spike/run-n3-pause.sh pause-qmp-recert-001 1000 qmp >> "$LOG" 2>&1 & P2=$!
wait "$P1"; R1=$?
wait "$P2"; R2=$?
{ [ $R1 -eq 0 ] && [ $R2 -eq 0 ]; } || fail "pause-pair (sigstop=$R1 qmp=$R2)"

step migrate-live
bash /root/nested-x86-spike/run-n3-migrate-live.sh migrate-live-recert-001 250 >> "$LOG" 2>&1 || fail migrate-live

# nested 10k hammer control at the metal-session seed (final_work equality)
step nested-control-10k
bash /root/nested-x86-spike/run-n2-condition.sh idle 10000 cond-idle-control10k-recert-001 2600001099 >> "$LOG" 2>&1 || fail nested-control-10k

echo "N3MATRIX NESTED_COMPLETE $(date -u +%FT%TZ)" | tee -a "$LOG"
echo "nested-complete" > "$STATE"
