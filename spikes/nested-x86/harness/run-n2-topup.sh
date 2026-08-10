#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# hm-dbh top-up (Paul's Option-A ruling 2026-07-15): drive 920,000 additional
# N-2 deadlines across the same condition matrix so CUMULATIVE armed PMIs
# (counted from perf records) reach >=1.05M. Sized at the measured ~55.4%
# armed rate: 920k deadlines -> ~510k armed PMIs -> cumulative ~1.099M.
set -uo pipefail
cd /root/nested-x86-spike
LOG=/root/nested-x86-recert/n2-topup.log
STATE=/root/nested-x86-recert/n2-topup.state
run_cond() { # run_cond <cond> <deadlines> <seed>
  local cond=$1 n=$2 seed=$3 rs="cond-$1-topup-001"
  echo "TOPUP_BEGIN $cond $n seed=$seed $(date -u +%FT%TZ)" | tee -a "$LOG"
  echo "running $cond" > "$STATE"
  bash /root/nested-x86-spike/run-n2-condition.sh "$cond" "$n" "$rs" "$seed" >> "$LOG" 2>&1
  local rc=$?
  echo "TOPUP_END $cond rc=$rc $(date -u +%FT%TZ)" | tee -a "$LOG"
  if [ $rc -ne 0 ]; then echo "FAILED $cond rc=$rc" > "$STATE"; exit $rc; fi
}
run_cond idle       350000 2600002002
run_cond othercore  175000 2600002004
run_cond samecore   130000 2600002006
run_cond mempress    90000 2600002008
run_cond timerstorm  90000 2600002010
run_cond migrate     85000 2600002012
echo "TOPUP_COMPLETE $(date -u +%FT%TZ)" | tee -a "$LOG"
echo "complete" > "$STATE"
