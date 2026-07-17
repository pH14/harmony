#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# build-kvm-amd.sh — AE-3 patched kvm_amd module build recipe (docs/AMD-EPYC.md AE-3).
# Runs ON the box. Content-pins the stock module first (record-then-modify), fetches the
# pinned kernel source, applies the vendor-neutral determinism plumbing (x86 patches
# 0001/0002/0004) plus the SVM force-exit hunk (patches/0004-KVM-SVM-...), builds the
# out-of-tree kvm/kvm-amd modules against the running kernel, and content-pins the result.
#
# It does NOT load the patched module by default (--load does): loading a rebuilt kvm_amd
# requires unloading the stock one, which drops any running VM. The stock module's sha256
# and srcversion are captured to results/ae-3/ FIRST so restore is exact.
#
# NOTE: this is the recipe; AE-3's acceptance (the deterministic exit actually fires with
# bounded skid, mechanism-attested) is measured by the KVM harness after --load, not by
# a successful build. A build that compiles is necessary, never sufficient (the PR-98
# lesson: a green build is not a green mechanism).
set -euo pipefail
SD=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SD/.." && pwd)
OUT="$ROOT/results/ae-3"; mkdir -p "$OUT"
KREL=$(uname -r)
KVER=${KREL%-generic}                       # e.g. 6.8.0-88
SRC="$HOME/kernel-src"

record_stock() {
  local m; m=$(modinfo -F filename kvm_amd)
  python3 - "$m" > "$OUT/stock-kvm_amd.json" <<PY
import json,subprocess,sys,hashlib
p=sys.argv[1]
raw=p
# modinfo may report a .zst-compressed path; hash the file as-is and record both.
h=hashlib.sha256(open(p,'rb').read()).hexdigest()
sv=subprocess.check_output(["modinfo","-F","srcversion","kvm_amd"]).decode().strip()
print(json.dumps({"schema":"amd-epyc-kvm_amd-identity-v1","role":"stock",
 "path":p,"sha256":h,"srcversion":sv,"kernel":sys.argv[0] if False else __import__("os").uname().release},
 sort_keys=True,indent=2))
PY
  echo "recorded stock kvm_amd identity -> $OUT/stock-kvm_amd.json" >&2
}

fetch_src() {
  [ -d "$SRC" ] && { echo "kernel source already at $SRC" >&2; return; }
  sudo apt-get -y -q install "linux-source-${KVER}" >/dev/null
  mkdir -p "$SRC"
  tar -C "$SRC" --strip-components=1 -xf "/usr/src/linux-source-${KVER}.tar.bz2"
  echo "fetched + unpacked linux-source-${KVER} -> $SRC" >&2
}

apply_patches() {
  cd "$SRC"
  cp "/boot/config-${KREL}" .config
  # the vendor-neutral determinism plumbing shared with the Intel backend:
  local KP="$ROOT/../../consonance/vmm-backend/kvm-patches/patches"
  git init -q 2>/dev/null || true; git add -A -q 2>/dev/null || true
  git -c user.email=s@s -c user.name=s commit -qm base 2>/dev/null || true
  for p in \
      "$KP/0001-KVM-x86-add-KVM_EXIT_DETERMINISM-userspace-exit-ABI.patch" \
      "$KP/0002-KVM-x86-emulate-intercepted-RDTSC-RDTSCP-RDRAND-RDSE.patch" \
      "$KP/0004-KVM-x86-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-pr.patch" \
      "$SD/patches/0004-KVM-SVM-KVM_EXIT_PREEMPT-force-exit-analogue.patch"; do
    echo "applying $(basename "$p")" >&2
    git apply --index "$p" || { echo "APPLY FAILED: $p (context drift vs ${KVER}; adjust hunk)" >&2; exit 1; }
    git -c user.email=s@s -c user.name=s commit -qm "$(basename "$p")"
  done
}

build() {
  cd "$SRC"
  make -j"$(nproc)" modules_prepare
  make -j"$(nproc)" M=arch/x86/kvm
  local ko="arch/x86/kvm/kvm-amd.ko"
  [ -f "$ko" ] || { echo "BUILD FAILED: $ko not produced" >&2; exit 1; }
  local h; h=$(sha256sum "$ko" | awk '{print $1}')
  python3 -c "import json;print(json.dumps({'schema':'amd-epyc-kvm_amd-identity-v1','role':'patched','path':'$SRC/$ko','sha256':'$h'},sort_keys=True,indent=2))" > "$OUT/patched-kvm_amd.json"
  echo "built patched kvm-amd.ko sha256=$h -> $OUT/patched-kvm_amd.json" >&2
}

load() {  # DANGER: drops running VMs. Content-verify before insmod (evidence integrity #3).
  cd "$SRC"
  local ko="arch/x86/kvm/kvm-amd.ko" want; want=$(python3 -c 'import json;print(json.load(open("'"$OUT"'/patched-kvm_amd.json"))["sha256"])')
  local have; have=$(sha256sum "$ko" | awk '{print $1}')
  [ "$want" = "$have" ] || { echo "HASH MISMATCH pre-load: refusing insmod" >&2; exit 1; }
  sudo rmmod kvm_amd 2>/dev/null || true
  sudo insmod "$SRC/arch/x86/kvm/kvm.ko" 2>/dev/null || true
  sudo insmod "$ko" avic=0
  echo "loaded patched kvm_amd (avic=0); AE-3 mechanism attestation is the harness's job" >&2
}

restore() {  # return to the stock signed module (baseline kvm_amd)
  sudo rmmod kvm_amd 2>/dev/null || true
  sudo modprobe kvm_amd
  echo "restored stock kvm_amd (baseline)" >&2
}

case "${1:-all}" in
  record) record_stock ;;
  fetch)  fetch_src ;;
  apply)  apply_patches ;;
  build)  record_stock; fetch_src; apply_patches; build ;;
  load)   load ;;
  restore) restore ;;
  all)    record_stock; fetch_src; apply_patches; build
          echo "build complete; run '$0 load' to insmod (drops running VMs), then the KVM harness attests the mechanism" >&2 ;;
  *) echo "usage: build-kvm-amd.sh record|fetch|apply|build|load|restore|all" >&2; exit 2 ;;
esac
