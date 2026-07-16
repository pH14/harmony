#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86 re-certification: the repo→box staging map, executable (PR #98
# round-3 #3 — a fresh recert run must be stageable from the committed
# checkout). Run FROM THE REPO ROOT on the workstation; SSH host `hetzner`.
#
#   bash spikes/nested-x86/harness/stage-to-box.sh
#
# Stages (matching the paths every committed driver invokes):
#   source tree            -> /root/harmony-nested            (sha256-verified git-archive)
#   appliance/*.sh         -> /root/nested-x86-spike/n1/src/
#   harness drivers        -> /root/nested-x86-spike/         (run-n2-condition, run-n3-*)
#   matrix/top-up drivers  -> /root/nested-x86-recert/        (run-n2-matrix, run-n2-topup,
#                                                              run-n3-matrix-recert)
# After staging: build gates (build-gates.sh writes
# /root/nested-x86-recert/gate-bins.txt), then build-appliance.sh, then smoke.
set -euo pipefail

HEAD=$(git rev-parse --short HEAD)
TB="/tmp/harmony-$HEAD.tar.gz"
git archive --format=tar.gz -o "$TB" HEAD
SUM=$(shasum -a 256 "$TB" | cut -d' ' -f1)
scp -q "$TB" "hetzner:/root/nested-x86-recert/harmony-$HEAD.tar.gz"
ssh hetzner "set -e
cd /root/nested-x86-recert
echo '$SUM  harmony-$HEAD.tar.gz' | sha256sum -c -
rm -rf src-$HEAD && mkdir src-$HEAD && tar -xzf harmony-$HEAD.tar.gz -C src-$HEAD
mkdir -p /root/harmony-nested /root/nested-x86-spike/n1/src
# --delete so re-staging can never leave stale source files from an earlier
# head in the tree (round-8 P1); build outputs and the commit stamp are
# excluded from deletion — they are box-produced state, not source, and the
# appliance manifest pins every artifact it bundles by sha256 regardless.
rsync -a --delete \
  --exclude /target/ --exclude /guest/payloads/target/ --exclude /guest/build/ \
  --exclude /guest/dl/ --exclude /.spike-source-commit \
  src-$HEAD/ /root/harmony-nested/
echo $HEAD > /root/harmony-nested/.spike-source-commit
# assert: every staged source file matches the recorded source commit
# (tar listing == staged tree, excluding the box-produced paths above)
tar -tzf harmony-$HEAD.tar.gz | grep -v '/$' | sort > /tmp/stage-manifest-$HEAD
( cd /root/harmony-nested && find . -type f \
    ! -path './target/*' ! -path './guest/payloads/target/*' \
    ! -path './guest/build/*' ! -path './guest/dl/*' ! -name .spike-source-commit \
    | sed 's|^\./||' | sort ) > /tmp/stage-actual-$HEAD
diff /tmp/stage-manifest-$HEAD /tmp/stage-actual-$HEAD > /dev/null \
  || { echo 'STAGE_MISMATCH: staged tree != source commit listing'; diff /tmp/stage-manifest-$HEAD /tmp/stage-actual-$HEAD | head -20; exit 1; }
echo 'STAGE_VERIFIED: staged tree == source commit listing'
SP=/root/harmony-nested/spikes/nested-x86
cp \$SP/appliance/build-appliance.sh \$SP/appliance/run-appliance.sh \$SP/appliance/l1-appliance-init.sh /root/nested-x86-spike/n1/src/
cp \$SP/harness/run-n2-condition.sh \$SP/harness/run-n3-stress.sh \$SP/harness/run-n3-pause.sh \$SP/harness/run-n3-migrate-live.sh \$SP/harness/run-metal-reference-recert.sh \$SP/harness/extract-probe-json.sh /root/nested-x86-spike/
cp \$SP/harness/run-n2-matrix.sh \$SP/harness/run-n2-topup.sh \$SP/harness/run-n3-matrix-recert.sh /root/nested-x86-recert/
mkdir -p /root/nested-x86-spike/n0/src
cp \$SP/l0/build-l1-probe.sh \$SP/l0/run-l1-probe.sh \$SP/l0/l1-init.sh \$SP/l0/probe.c /root/nested-x86-spike/n0/src/
echo STAGED $HEAD"
