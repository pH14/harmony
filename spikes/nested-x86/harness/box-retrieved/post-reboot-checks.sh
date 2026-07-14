#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
# 1. N-0 capability identity after L0 reboot (runset-005)
bash /root/nested-x86-spike/n0/src/run-l1-probe.sh runset-005
# 2. N-2 count exactness + N-3 hash stability after L0 reboot
bash /root/nested-x86-spike/n1/src/run-appliance.sh /root/nested-x86-spike/n3/results/post-reboot-001 3600 \
  "harmony.gates=n2_nested_hammer,n3_repeat_gate harmony.env=N2_DEADLINES=10000,N3_REPS=100,N3_ITEM=insn-rng"
echo POST_REBOOT_CHECKS_DONE
