# `host/` — the Harmony arm64 KVM patch series

**UNTESTED ON SILICON. DRAFT.** This directory holds the arm64 analogue of the
x86 "0004" determinism patch, the AA-4 stage-2 execute guard, and the machinery to
prove they *apply and compile* against a pinned kernel tree. It has **never produced a booted host kernel**, made
no measurement, and asserts nothing about runtime behavior — correctness is stage
AA-3's, on real Neoverse N1 (`docs/ARM-ALTRA.md`).

| File | What it is |
|---|---|
| `patches/0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch` | the draft patch, `git format-patch` against pristine `linux-6.18.35` |
| `patches/0002-KVM-arm64-add-userspace-stage-2-execute-guard.patch` | the draft AA-4 default-XN/scan/approve/write-revoke state machine |
| `patches/README.md` | the mechanism, the exit/ioctl/cap ABI, the arch-difference residual, and the AA-3 refinement question |
| `BUILD.md` | the copy-pasteable apply → configure → build recipe with the pinned source |
| `verify.sh` | the automated gate: reset to pristine, `git am`, build `arch/arm64/kvm/`, and **assert the mechanism is in the compiled objects** — all with RC propagation |

## What the patches do

It is the arm64 mirror of `consonance/vmm-backend/kvm-patches/patches/0004-*` — the
in-kernel force-exit that turns a guest-mode work-counter overflow into a
deterministic vCPU exit with a dedicated exit reason. A per-vCPU one-shot
(`preempt_armed`), armed by a `KVM_ARM_PREEMPT_EXIT` ioctl and gated on a per-VM
`KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS` opt-in (default-off ⇒ stock behavior),
causes `handle_exit()`'s IRQ path to return to userspace with `KVM_EXIT_PREEMPT`
instead of re-entering. UAPI numbers (`KVM_EXIT_PREEMPT = 42`,
`KVM_ARM_PREEMPT_EXIT = _IO(KVMIO, 0xe4)`) match the x86 series deliberately —
`include/uapi/linux/kvm.h` is arch-independent, so the two ABIs are one ABI.

Patch `0002` adds the opt-in `KVM_CAP_ARM_STAGE2_EXEC_GUARD = 246`. Every GFN begins
writable/execute-never. First execute freezes the backing and exits with a non-reused scan
generation; userspace must approve that exact generation before the page becomes
executable/read-only. A later write synchronously revokes execute and exits before the store.
MMU-notifier replacement and memslot reuse clear approvals, page-sized mappings prevent a block
mapping from bypassing the state machine, and final approval is serialized with the MMU write
lock. The trusted VMM boundary is explicit: unique anonymous backing, no DMA-capable assigned
device, and no direct host write to an approved page without revocation.

That is only a compile-verified kernel mechanism. The harness now has the userspace half:
`linux-boot --stage2-exec-guard` scans and approves clean pages while requiring nonzero guard
statistics, and `aa4-guard-reject` requires a hash-pinned exclusive-bearing page to be rejected
while its PC remains unexecuted. Those paths are also pre-silicon. AA-4 remains
cooperative-residual until the patch is booted on the pinned N1 and live proofs demonstrate first
execute, approve/reject, stale-generation rejection, exit-before-write, scan-racing writes, and
backing replacement. No compile result or unrun VMM path is promoted to that runtime claim.

## The load-bearing arch difference (why this is a *draft*, not a port)

On **x86**, the perf-overflow PMI is an **NMI**, and `handle_exception_nmi()` is an
NMI-specific exit path — so `KVM_EXIT_PREEMPT` fires nearly unambiguously on the
work-counter overflow.

On **arm64**, the PMU overflow arrives as an ordinary **maskable IRQ**, and *every*
host IRQ (the host timer tick, IPIs, device interrupts) leaves the guest through
the single `ARM_EXCEPTION_IRQ` path. So an armed vCPU exits `KVM_EXIT_PREEMPT` on
**any** host IRQ, not only on the work-counter overflow. This is a **named
residual, not a defect**: userspace re-reads the work counter and, if the deadline
was not reached, re-arms and re-enters — exactly what the x86 backend already does
for stale `KVM_EXIT_PREEMPT` exits (`consonance/vmm-backend/kvm-patches/patches/README.md`
§"Disarm asymmetry"; `decode_exit` in `consonance/vmm-backend/src/kvm.rs` swallows
them as transparent re-entry). The bounded-skid claim is unaffected; only
efficiency is, and core isolation / `nohz_full` on the pinned measurement core
mitigates it.

**The refinement AA-3 must evaluate (and this patch deliberately does not
implement):** move the work counter in-kernel via
`perf_event_create_kernel_counter()` with an overflow handler that sets a
`preempt_pending` flag directly, so the exit is precise instead of
IRQ-ambiguous — at the cost of new counter-configuration ioctls. That is a design
decision for stage AA-3 with silicon in hand, not an offline call.

## Another arrival-day fact: arm64 KVM is built-in

Unlike x86 (where `kvm`/`kvm-intel` are modules and the determinism spike
hot-swaps them with `insmod`), arm64 KVM is `CONFIG_KVM=y` — **built into the
kernel image**. There is no `.ko` to swap: the patched host kernel must be *booted*,
so every AA-3 test cycle costs a reboot. `BUILD.md` states this; it is a real cost
in the stage's execution budget, not a footnote.

## Running the gate

```sh
# Prereq: a native-aarch64 Linux builder with the pinned tree at /work/linux-6.18.35,
# tagged v6.18.35-pristine, reachable as a docker container named `armk` (or set
# ARMK_CONTAINER). BUILD.md §0 has the one-time setup.
./verify.sh          # PASS only if: applies clean, builds clean, mechanism in the objects
```

`verify.sh` is written to the same evidence-integrity standard as everything else
here: `set -euo pipefail`, every step's RC reaches the script's RC, no `|| true` on
any gate, and the final check disassembles the built `arm.o`, `handle_exit.o`, and `mmu.o`. It
asserts the AA-3 ioctl/cap/flag/exit and the AA-4 capability/ioctl/flag/exit plus the compiled
XArray, MMU-write-lock, synchronous-unmap, and range-invalidation calls. A source grep would
pass on a patch that compiled to nothing.
A "reached the end" print is never the success condition.
