#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# verify.sh — apply + build + assert gate for the Harmony arm64 KVM patch series.
# Untested-on-silicon: this proves "applies + compiles", nothing more.
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
PATCH_NAMES=(
	"0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch"
	"0002-KVM-arm64-add-userspace-stage-2-execute-guard.patch"
)
PATCH_CONTAINER_PATHS=()
BUILD_LOG_CONTAINER="/tmp/verify-build.log"
BUILD_LOG_HOST="$(mktemp -t verify-build-log.XXXXXX)"

trap 'rm -f "${BUILD_LOG_HOST}"' EXIT

echo "==> [0/6] preflight: container reachable, patch files present"
for patch_name in "${PATCH_NAMES[@]}"; do
	patch_host_path="${SCRIPT_DIR}/patches/${patch_name}"
	if [[ ! -f "${patch_host_path}" ]]; then
		echo "FAIL: patch not found at ${patch_host_path}" >&2
		exit 1
	fi
	PATCH_CONTAINER_PATHS+=("/tmp/verify-${patch_name}")
done
docker exec "${CONTAINER}" true

echo "==> [1/6] copying patch series into ${CONTAINER}"
for i in "${!PATCH_NAMES[@]}"; do
	docker cp "${SCRIPT_DIR}/patches/${PATCH_NAMES[$i]}" \
		"${CONTAINER}:${PATCH_CONTAINER_PATHS[$i]}"
done

echo "==> [2/6] resetting ${KERNEL_DIR} to pristine ${PRISTINE_TAG}"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" "${PRISTINE_TAG}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
PRISTINE_TAG="$2"
cd "${KERNEL_DIR}"
git am --abort >/dev/null 2>&1 || true
git reset --hard "${PRISTINE_TAG}"
EOF

echo "==> [3/6] git am the series (must apply clean onto pristine ${PRISTINE_TAG})"
docker exec -i "${CONTAINER}" bash -s -- "${KERNEL_DIR}" "${PATCH_CONTAINER_PATHS[@]}" <<'EOF'
set -euo pipefail
KERNEL_DIR="$1"
shift
cd "${KERNEL_DIR}"
git -c user.name=spike -c user.email=s@s am "$@"
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
objdump -dr arch/arm64/kvm/mmu.o        > /tmp/verify-mmu.dis
nm arch/arm64/kvm/mmu.o                 > /tmp/verify-mmu.nm

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

# (e) The execute-guard capability (246 / 0xf6) and response ioctl
#     _IOW(KVMIO, 0xb7, 24) == 0x4018aeb7 are dispatched by arm.o.
grep -Eq 'cmp[[:space:]]+w[0-9]+, #0xf6' /tmp/verify-arm.dis \
	|| fail "cap 246 (0xf6) dispatch not found in arm.o's enable_cap switch"
grep -Eq 'mov[[:space:]]+w[0-9]+, #0xaeb7' /tmp/verify-arm.dis \
	|| fail "ioctl low half 0xaeb7 not found in arm.o"
grep -Eq 'movk[[:space:]]+w[0-9]+, #0x4018, lsl #16' /tmp/verify-arm.dis \
	|| fail "ioctl high half 0x4018 not found in arm.o"

# (f) The page-state implementation and exit are linked into mmu.o. The named apply
#     entry point must exist; the object must test flag bit 12, emit exit reason 43,
#     serialize under the MMU write lock, load XArray generation state, synchronously
#     unmap stage-2 mappings, and clear state ranges for notifier/memslot invalidation.
grep -q ' T kvm_arm_stage2_exec_guard_apply$' /tmp/verify-mmu.nm \
	|| fail "kvm_arm_stage2_exec_guard_apply not exported by mmu.o"
grep -Eq '(tbz|tbnz)[[:space:]]+w[0-9]+, #0xc,' /tmp/verify-mmu.dis \
	|| fail "bit-12 (KVM_ARCH_FLAG_STAGE2_EXEC_GUARD) test not found in mmu.o"
grep -Eq 'mov[[:space:]]+w[0-9]+, #0x2b[[:space:]]+// #43' /tmp/verify-mmu.dis \
	|| fail "KVM_EXIT_ARM_STAGE2_EXEC_GUARD (43 / 0x2b) not found in mmu.o"
for relocation in _raw_write_lock xa_load __unmap_stage2_range xa_store_range; do
	grep -q "R_AARCH64_CALL26[[:space:]]${relocation}" /tmp/verify-mmu.dis \
		|| fail "${relocation} call not found in mmu.o"
done

echo "    all compiled-object assertions passed (AA-3 force-exit + AA-4 execute guard)"
EOF

echo "PASS"
