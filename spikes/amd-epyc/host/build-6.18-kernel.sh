#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# build-6.18-kernel.sh — AE-3 (Paul ruling 2026-07-17): build a BOOTABLE patched
# linux-6.18.35 determinism kernel ON the box (harmony-amd) and boot into it.
#
# WHY THIS SUPERSEDES build-kvm-amd.sh. That script built the kvm_amd module
# OUT-OF-TREE against the RUNNING stock 6.8 kernel — which is exactly why AE-3
# escalated: the KVM_EXIT_PREEMPT infrastructure (deterministic_intercepts /
# preempt_armed / the KVM_EXIT_PREEMPT UAPI value) is added by the ~6.18
# determinism series (patches 0001/0002/0004), absent from stock 6.8, and a 6.18
# kvm.ko cannot be insmod'd into a 6.8 host (vermagic + modversions mismatch).
# The only sound path is to build+boot the full 6.18.35 kernel so the patched
# kvm/kvm-amd modules match the running kernel. Ruled by Paul 2026-07-17: build on
# THIS box (a fresh Scaleway lease is blocked by hm-3cp and buys nothing — no stock
# distro carries the determinism patches, so the build is identical either way).
#
# BOOT-SAFETY (this exact box). Root is on /dev/md1 (software RAID1) over NVMe and
# /boot is a tight ~469M md0 RAID1. A vanilla `make defconfig` kernel would not
# find the MD/RAID1 + NVMe drivers and would fail to mount root. This recipe
# therefore bases .config on the RUNNING kernel's Ubuntu config (inherits every
# driver the box actually boots on) and uses a MODULES=dep initramfs to fit /boot.
# The boot itself is guarded by host/stage-6.18-boot.sh (self-recovering GRUB
# one-shot; panic=30 auto-fallback to the stock 6.8 kernel).
#
# The build compiles; a green build is necessary, never sufficient (the PR-98
# lesson). AE-3's acceptance — the deterministic exit actually fires with bounded
# skid, mechanism-attested — is measured by the KVM harness AFTER boot+load, not
# by a successful build.
set -euo pipefail

KVER=6.18.35
KSHA=f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236
KURL=https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.35.tar.xz

SD=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORK="${KBUILD_WORK:-$HOME/kbuild-618}"
SRC="$WORK/linux-$KVER"
PATCHES="$WORK/patches"                       # staged: 0001-0005 canonical + AMD svm.c hunk
OUT="${AE3_OUT:-$HOME/amd-epyc-spike/results/ae-3}"
RUNREL=$(uname -r)                            # 6.8.0-88-generic (the restore target)
mkdir -p "$WORK" "$OUT"

# Empty LOCALVERSION in the env makes scripts/setlocalversion skip the git-ahead
# "+" suffix (the patched HEAD is 6 commits past the tag), so `uname -r`, the .deb
# name, and /lib/modules/<rel> are all exactly 6.18.35 (Paul's verify). The
# in-tree .scmversion is belt-and-suspenders; this env var is the load-bearing fix.
export LOCALVERSION=

log(){ echo "[build-6.18] $*" >&2; }

deps() {
  sudo apt-get -y -q update >/dev/null 2>&1 || true
  # Everything the kernel build + bindeb-pkg + a dep-mode initramfs needs. Most are
  # already present on this box; install is idempotent.
  sudo apt-get -y -q install build-essential bc bison flex libssl-dev libelf-dev \
    libdw-dev dwarves rsync cpio kmod zstd debhelper >/dev/null 2>&1 || true
  log "build deps ensured"
}

record_stock() {
  local m; m=$(modinfo -F filename kvm_amd)
  python3 - "$m" "$RUNREL" > "$OUT/stock-kvm_amd.json" <<'PY'
import json,subprocess,sys,hashlib,os
p,rel=sys.argv[1],sys.argv[2]
h=hashlib.sha256(open(p,'rb').read()).hexdigest()
sv=subprocess.check_output(["modinfo","-F","srcversion","kvm_amd"]).decode().strip()
vm=subprocess.check_output(["modinfo","-F","vermagic","kvm_amd"]).decode().strip()
print(json.dumps({"schema":"amd-epyc-kvm_amd-identity-v1","role":"stock",
 "path":p,"sha256":h,"srcversion":sv,"vermagic":vm,"kernel":rel},
 sort_keys=True,indent=2))
PY
  log "recorded stock kvm_amd identity -> $OUT/stock-kvm_amd.json"
}

fetch() {
  cd "$WORK"
  if [ ! -f "linux-$KVER.tar.xz" ]; then
    curl -fsSL -o "linux-$KVER.tar.xz" "$KURL"
  fi
  echo "$KSHA  linux-$KVER.tar.xz" | sha256sum -c -   # content-pin gate (evidence integrity #3)
  log "source tarball sha256 verified against the pin"
  [ -d "$SRC" ] || tar -C "$WORK" -xf "linux-$KVER.tar.xz"
  cd "$SRC"
  # Guard the pristine setup on the TAG (not the dir) so a half-initialized tree
  # re-baselines cleanly. This box's `git add` rejects -q — omit it.
  if ! git rev-parse -q --verify "refs/tags/v$KVER-pristine" >/dev/null 2>&1; then
    rm -rf .git
    git init -q
    git add -A
    git -c user.name=spike -c user.email=s@s commit -q -m "v$KVER pristine"
    git tag "v$KVER-pristine"
    log "extracted + git-tagged v$KVER-pristine"
  fi
}

# Populate $PATCHES from the IN-REPO series so a fresh checkout reproduces the build
# (P1-4). The canonical determinism series lives in the tree; the AMD hunk lives beside
# this script and is staged as amd-svm.patch (so the 0004-*.patch glob can't grab it).
stage_patches() {
  mkdir -p "$PATCHES"
  local series="$SD/../../../consonance/vmm-backend/kvm-patches/patches"
  local amd="$SD/patches/0004-KVM-SVM-KVM_EXIT_PREEMPT-force-exit-analogue.patch"
  if [ -d "$series" ] && [ -f "$amd" ]; then
    cp "$series"/0001-*.patch "$series"/0002-*.patch "$series"/0003-*.patch \
       "$series"/0004-*.patch "$series"/0005-*.patch "$PATCHES"/
    cp "$amd" "$PATCHES/amd-svm.patch"
    log "staged canonical series 0001-0005 + AMD hunk into $PATCHES from the in-repo tree"
  elif ls "$PATCHES"/0001-*.patch >/dev/null 2>&1 && [ -f "$PATCHES/amd-svm.patch" ]; then
    log "in-repo series absent (standalone box copy); using pre-staged patches in $PATCHES"
  else
    log "cannot locate the determinism series: neither in-repo ($series) nor pre-staged ($PATCHES)"
    exit 3
  fi
}

apply() {
  stage_patches
  cd "$SRC"
  git config user.email s@s; git config user.name spike   # git am needs an identity
  # Re-baseline to pristine so re-runs are idempotent.
  git checkout -q -f "v$KVER-pristine"
  git clean -qfdx -e .config 2>/dev/null || true
  # Canonical x86 determinism series (git-am-clean onto pristine 6.18.35 per
  # consonance/vmm-backend/kvm-patches/BUILD.md — task 57's verified application).
  # Apply the full 0001-0005; 0003/0005 add only VMX (Intel) paths, inert for an
  # AMD run but keep the application byte-identical to the verified canonical tree.
  git am "$PATCHES"/0001-*.patch "$PATCHES"/0002-*.patch "$PATCHES"/0003-*.patch \
         "$PATCHES"/0004-*.patch "$PATCHES"/0005-*.patch
  log "canonical determinism series 0001-0005 applied (git am clean)"
  # The AMD/SVM 0004-analogue hunk (spikes/amd-epyc/host/patches, staged as
  # amd-svm.patch so the canonical 0004-*.patch glob above cannot pick it up).
  # Context-anchored on nmi_interception(); if line drift vs 6.18.35 defeats git
  # apply, STOP and re-anchor rather than force — never route around with fuzz.
  local amd="$PATCHES/amd-svm.patch"
  if git apply --index "$amd"; then
    git -c user.name=spike -c user.email=s@s commit -q -m "AMD SVM KVM_EXIT_PREEMPT analogue (AE-3)"
    log "AMD svm.c 0004-analogue applied clean"
  else
    log "AMD svm.c hunk did NOT apply to $KVER — nmi_interception context drift:"
    grep -n "nmi_interception" arch/x86/kvm/svm/svm.c >&2 || true
    log "STOP: re-anchor the hunk against $KVER svm.c (do not force-fuzz)"
    exit 3
  fi
}

configure() {
  cd "$SRC"
  cp "/boot/config-$RUNREL" .config           # inherit the box's booting driver set
  # Empty .scmversion suppresses scripts/setlocalversion's git-ahead "+" suffix so
  # `uname -r` is exactly 6.18.35 (the patched HEAD is 6 commits ahead of the tag).
  : > "$SRC/.scmversion"
  # KVM + KVM_AMD as loadable modules (so patched-vs-stock identity is attestable,
  # and we can content-pin the .ko). Disable the Ubuntu-config build traps:
  #  - SYSTEM_TRUSTED_KEYS / REVOCATION_KEYS point at canonical cert files absent
  #    from the vanilla tree (build dies at the cert step) -> clear them.
  #  - MODULE_SIG* off: we load an unsigned self-built module.
  #  - DEBUG_INFO*/BTF off: faster, smaller, no pahole dependency.
  #  - LOCALVERSION empty + AUTO off: uname -r is exactly 6.18.35 (Paul's verify).
  scripts/config \
    -e VIRTUALIZATION -m KVM -m KVM_AMD -e KVM_AMD_SEV \
    --set-str SYSTEM_TRUSTED_KEYS "" --set-str SYSTEM_REVOCATION_KEYS "" \
    -d MODULE_SIG -d MODULE_SIG_ALL -d MODULE_SIG_FORCE \
    -d DEBUG_INFO_BTF -d DEBUG_INFO_BTF_MODULES \
    -d DEBUG_INFO -e DEBUG_INFO_NONE \
    --set-str LOCALVERSION "" -d LOCALVERSION_AUTO
  make olddefconfig
  # Assert the release string is exactly the pinned version (no +/-dirty suffix).
  local kr; kr=$(make -s kernelrelease)
  [ "$kr" = "$KVER" ] || { log "kernelrelease=$kr != $KVER (localversion leak) — fix before build"; exit 4; }
  grep -qE '^CONFIG_KVM_AMD=m' .config || { log "KVM_AMD not =m"; exit 4; }
  log "configured: kernelrelease=$kr, KVM_AMD=m, BTF/DEBUG/SIG off, certs cleared"
}

build() {
  cd "$SRC"
  # bindeb-pkg produces linux-image + linux-headers .debs whose install hooks run
  # update-initramfs (MODULES=dep, set by stage-6.18-boot.sh) and update-grub.
  make -j"$(nproc)" bindeb-pkg           # full output to caller's log (evidence)
  local deb; deb=$(ls "$WORK"/linux-image-"$KVER"_*.deb | head -1)
  [ -f "$deb" ] || { log "BUILD FAILED: no linux-image .deb produced"; exit 5; }
  local dh; dh=$(sha256sum "$deb" | awk '{print $1}')
  # Content-pin the patched kvm-amd.ko out of the build tree too (attestation).
  local ko="$SRC/arch/x86/kvm/kvm-amd.ko"
  local kh=""; [ -f "$ko" ] && kh=$(sha256sum "$ko" | awk '{print $1}')
  python3 - "$deb" "$dh" "$ko" "$kh" "$KVER" > "$OUT/patched-kernel.json" <<'PY'
import json,sys
deb,dh,ko,kh,ver=sys.argv[1:6]
print(json.dumps({"schema":"amd-epyc-patched-kernel-v1","kernelrelease":ver,
 "image_deb":deb,"image_deb_sha256":dh,"kvm_amd_ko":ko,"kvm_amd_ko_sha256":kh},
 sort_keys=True,indent=2))
PY
  log "built linux-image .deb sha256=$dh -> $OUT/patched-kernel.json"
  log "NEXT: host/stage-6.18-boot.sh install (content-verifies the .deb, stages the self-recovering one-shot)"
}

case "${1:-all}" in
  deps)   deps ;;
  record) record_stock ;;
  fetch)  fetch ;;
  stage-patches) stage_patches ;;   # populate $PATCHES from the in-repo series (repro check)
  apply)  apply ;;
  config) configure ;;
  build)  build ;;
  prep)   deps; record_stock; fetch; apply; configure ;;   # everything up to (not incl.) the long compile
  all)    deps; record_stock; fetch; apply; configure; build ;;
  *) echo "usage: build-6.18-kernel.sh deps|record|fetch|stage-patches|apply|config|build|prep|all" >&2; exit 2 ;;
esac
