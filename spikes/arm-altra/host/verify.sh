#!/usr/bin/env bash
# verify.sh — apply + build + assert gate for the arm64 KVM_EXIT_PREEMPT draft patch
# (harmony task 109). Untested-on-silicon: this proves "applies + compiles", nothing more.
#
# Every step's exit status reaches this script's exit status (set -euo pipefail; no `|| true`
# on any gate step). A "done" print is never treated as success — the PASS line at the bottom
# only executes if every prior command in this script returned 0. Run from anywhere; paths are
# resolved relative to this script's own location.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTAINER="${ARMK_CONTAINER:-armk}"
KERNEL_DIR="/work/linux-6.18.35"
PRISTINE_TAG="v6.18.35-pristine"
PATCH_NAME="0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch"
PATCH_HOST_PATH="${SCRIPT_DIR}/patches/${PATCH_NAME}"
PATCH_CONTAINER_PATH="/tmp/verify-${PATCH_NAME}"
BUILD_LOG_CONTAINER="/tmp/verify-build.log"
BUILD_LOG_HOST="$(mktemp -t verify-build-log.XXXXXX)"

trap 'rm -f "${BUILD_LOG_HOST}"' EXIT

echo "==> [0/6] preflight: container reachable, patch file present"
if [[ ! -f "${PATCH_HOST_PATH}" ]]; then
	echo "FAIL: patch not found at ${PATCH_HOST_PATH}" >&2
	exit 1
fi
docker exec "${CONTAINER}" true

echo "==> [1/6] copying patch into ${CONTAINER}:${PATCH_CONTAINER_PATH}"
docker cp "${PATCH_HOST_PATH}" "${CONTAINER}:${PATCH_CONTAINER_PATH}"

echo "==> [2/6] resetting ${KERNEL_DIR} to pristine ${PRISTINE_TAG}"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" "${PRISTINE_TAG}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
PRISTINE_TAG="$2"
cd "${KERNEL_DIR}"
git am --abort >/dev/null 2>&1 || true
git reset --hard "${PRISTINE_TAG}"
EOF

echo "==> [3/6] git am the patch (must apply clean onto pristine ${PRISTINE_TAG})"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" "${PATCH_CONTAINER_PATH}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
PATCH="$2"
cd "${KERNEL_DIR}"
git -c user.name=spike -c user.email=s@s am "${PATCH}"
EOF

echo "==> [4/6] configuring (defconfig; KVM/VIRTUALIZATION on; DEBUG_INFO_BTF + DEBUG_INFO off)"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
cd "${KERNEL_DIR}"
make ARCH=arm64 defconfig >/dev/null
# CONFIG_DEBUG_INFO is a derived symbol re-selected by the "Debug information" choice group's
# active member (DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT by default on this defconfig) -- disabling
# it for real requires flipping the choice, not just `-d DEBUG_INFO` (see host/BUILD.md's
# caveat, found while building this gate for real).
scripts/config -e VIRTUALIZATION -e KVM \
	-d DEBUG_INFO_BTF \
	-d DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT -e DEBUG_INFO_NONE
make ARCH=arm64 olddefconfig >/dev/null
grep -q '^CONFIG_KVM=y$' .config
grep -q '^CONFIG_DEBUG_INFO_NONE=y$' .config
! grep -q '^CONFIG_DEBUG_INFO_BTF=y$' .config
EOF

echo "==> [5/6] building arch/arm64/kvm/ (RC must be 0, log must have zero new warnings/errors)"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" "${BUILD_LOG_CONTAINER}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
LOG="$2"
cd "${KERNEL_DIR}"
make ARCH=arm64 -j"$(nproc)" arch/arm64/kvm/ > "${LOG}" 2>&1
EOF
docker cp "${CONTAINER}:${BUILD_LOG_CONTAINER}" "${BUILD_LOG_HOST}"
if grep -iE 'warning:|error:' "${BUILD_LOG_HOST}"; then
	echo "FAIL: build log contains warnings or errors (see above)" >&2
	exit 1
fi
echo "    build log clean: $(wc -l < "${BUILD_LOG_HOST}") lines, no warning:/error: matches"

echo "==> [6/6] asserting the patched mechanism is present in the compiled objects (not source grep)"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
cd "${KERNEL_DIR}"

fail() { echo "FAIL: $1" >&2; exit 1; }

# Disassemble to real files first, not a live pipe into grep -q/-c: under `set -o pipefail`,
# an early-exiting grep (-q, or -m1) closes its read end and sends objdump a SIGPIPE, which
# makes the *pipeline's* exit status non-zero even though grep itself matched -- a false
# FAIL. Disassembling to a file first removes the producer process from the equation.
objdump -d arch/arm64/kvm/arm.o         > /tmp/verify-arm.dis
objdump -d arch/arm64/kvm/handle_exit.o > /tmp/verify-handle_exit.dis

# (a) KVM_ARM_PREEMPT_EXIT ioctl number -- _IO(KVMIO, 0xe4) == (0xAE << 8) | 0xe4 == 0xaee4 --
#     dispatched as a literal compare in kvm_arch_vcpu_ioctl's switch, compiled into arm.o.
grep -q '0xaee4' /tmp/verify-arm.dis \
	|| fail "0xaee4 (KVM_ARM_PREEMPT_EXIT ioctl number) not found in arm.o"

# (b) KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS == 245 == 0xf5 -- dispatched in the enable_cap switch.
grep -Eq 'cmp[[:space:]]+w[0-9]+, #0xf5' /tmp/verify-arm.dis \
	|| fail "cap 245 (0xf5) dispatch not found in arm.o's enable_cap switch"

# (c) KVM_ARCH_FLAG_DETERMINISTIC_INTERCEPTS == bit 11 of kvm->arch.flags -- tested in BOTH
#     arm.o (the arm ioctl) and handle_exit.o (the force-exit itself). Both must be present:
#     one without the other means the gate (arm) or the fire (exit) compiled out.
[[ "$(grep -cE 'tbz[[:space:]]+w[0-9]+, #11,' /tmp/verify-arm.dis)" -ge 1 ]] \
	|| fail "bit-11 (KVM_ARCH_FLAG_DETERMINISTIC_INTERCEPTS) test not found in arm.o"
[[ "$(grep -cE 'tbz[[:space:]]+w[0-9]+, #11,' /tmp/verify-handle_exit.dis)" -ge 1 ]] \
	|| fail "bit-11 (KVM_ARCH_FLAG_DETERMINISTIC_INTERCEPTS) test not found in handle_exit.o"

# (d) KVM_EXIT_PREEMPT == 42 == 0x2a -- the literal actually written to run->exit_reason in the
#     patched ARM_EXCEPTION_IRQ case. This is the single most direct proof the force-exit path
#     compiled in: the constant appears nowhere else in handle_exit.c.
grep -Eq 'mov[[:space:]]+w[0-9]+, #0x2a[[:space:]]+// #42' /tmp/verify-handle_exit.dis \
	|| fail "KVM_EXIT_PREEMPT (42 / 0x2a) literal not found in handle_exit.o"

echo "    all 4 compiled-object assertions passed (0xaee4, 0xf5, bit-11 x2, 0x2a/#42)"
EOF

echo "PASS"
