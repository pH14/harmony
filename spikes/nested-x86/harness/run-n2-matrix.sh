#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# hm-dbh: N-2 re-run on the PATCHED mechanism — serialized condition matrix.
# >=1,000,000 armed deadlines cumulative; distinct high-bit-spaced seeds (the
# original seed|1 collapse is avoided by spacing seeds by 2).
set -uo pipefail
cd /root/nested-x86-spike
LOG=/root/nested-x86-recert/n2-matrix.log
STATE=/root/nested-x86-recert/n2-matrix.state
run_cond() { # run_cond <cond> <deadlines> <seed>
  local cond=$1 n=$2 seed=$3 rs="cond-$1-recert-001"
  echo "MATRIX_BEGIN $cond $n seed=$seed $(date -u +%FT%TZ)" | tee -a "$LOG"
  echo "running $cond" > "$STATE"
  bash /root/nested-x86-spike/run-n2-condition.sh "$cond" "$n" "$rs" "$seed" >> "$LOG" 2>&1
  local rc=$?
  echo "MATRIX_END $cond rc=$rc $(date -u +%FT%TZ)" | tee -a "$LOG"
  if [ $rc -ne 0 ]; then
    echo "FAILED $cond rc=$rc" > "$STATE"
    exit $rc   # stop the matrix on first failure — diagnose before spending more
  fi
}
run_cond idle       400000 2600001002
run_cond othercore  200000 2600001004
run_cond samecore   150000 2600001006
run_cond mempress   100000 2600001008
run_cond timerstorm 100000 2600001010
run_cond migrate    100000 2600001012
echo "MATRIX_COMPLETE $(date -u +%FT%TZ)" | tee -a "$LOG"
echo "complete" > "$STATE"
