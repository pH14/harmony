#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# provision.sh — recorded modification #1 (after the pristine baseline capture,
# docs/AMD-EPYC.md §Box discipline record-then-modify). Idempotent. Installs the
# build/measurement toolchain the spike needs; changes NO measurement posture
# (governor, NMI watchdog, LS_CFG, SMT siblings stay baseline — posture.sh owns
# those per-run, attested and restored).
#
# Runs ON the box. Log + RC land beside it: ~/amd-epyc-spike/provision.{log,rc}
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
APT="sudo apt-get -o DPkg::Lock::Timeout=600 -y -q"

$APT update
# build-essential: gcc/make for the C hammer + kernel module builds (AE-3)
# msr-tools: rdmsr/wrmsr for LS_CFG posture (bit 54, the SpecLockMap workaround)
# stress-ng: AE-1 contamination probes (co-tenant load conditions)
# jq: evidence plumbing in shell drivers
# flex/bison/libelf-dev/libssl-dev/dwarves/bc: kernel module build deps (AE-3 svm.c proxy)
$APT install build-essential msr-tools stress-ng jq flex bison libelf-dev libssl-dev dwarves bc

if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi
. "$HOME/.cargo/env" 2>/dev/null || true
rustc --version
gcc --version | head -1
rdmsr --version 2>&1 | head -1 || true
stress-ng --version | head -1
echo PROVISION_OK
