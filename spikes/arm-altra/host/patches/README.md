# arm64 KVM_EXIT_PREEMPT patch — DRAFT, untested on silicon

Status: **draft, harmony task 109 (ARM pre-build apparatus).** This is the arm64 analogue of
the x86 in-kernel force-exit preemption patch
(`consonance/vmm-backend/kvm-patches/patches/0004-KVM-x86-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-pr.patch`,
see that directory's `README.md`), built and verified to `git am` + compile against a pristine
`linux-6.18.35` tree on a native aarch64 build container. It has never run as a host kernel and
has never booted a guest. Per `docs/ARM-ALTRA.md` stage AA-3, this patch is exactly the "real
arm64 KVM patch work" that stage validates on real Ampere Altra silicon — this deliverable is
the pre-silicon draft the pre-build ruling (`docs/ARCH-BOUNDARY.md` §Pre-build ruling) allows to
exist before that GO/NO-GO. See `../BUILD.md` for the apply+build recipe and `../verify.sh` for
the automated apply+build+assert gate.

AA-4's runtime execute guard is a separate, currently missing kernel extension. Stock arm64 KVM
does not expose per-GFN stage-2 XN state or execute faults to userspace; it resolves instruction
faults by granting execute internally. The harness reserves
`KVM_CAP_ARM_STAGE2_EXEC_GUARD = 246` as an expect-absent marker only. Presence must eventually
mean the full non-vacuous state machine: default XN, exit before first execute, userspace scan,
approve executable/read-only, and exit-before-write to revoke execute. No current patch in this
directory advertises it.

## What the patch does

Mirrors the x86 mechanism exactly, mechanism-for-mechanism:

- x86: a retired-branch perf_event armed at `deadline - SKID_MARGIN` overflows and fires a host
  PMI, delivered as an **NMI**. `PIN_BASED_NMI_EXITING` makes the NMI VM-exit;
  `handle_exception_nmi()` sees the NMI was already serviced by `vmx_vcpu_enter_exit()` and, if
  armed, returns to userspace with `KVM_EXIT_PREEMPT` instead of re-entering.
- arm64: V-time is driven by the same class of retired-branch overflow (`BR_RETIRED`, raw event
  `0x21` on Neoverse N1 — `docs/ARM-ALTRA.md` §2), but the PMU overflow interrupt is delivered as
  an ordinary **maskable IRQ**. arm64 KVM has no PMI-specific exit path; every guest-mode host
  IRQ is reported through the single `ARM_EXCEPTION_IRQ` case of `handle_exit()`
  (`arch/arm64/kvm/handle_exit.c`). This patch hooks that case: if the VM opted in and the
  one-shot arm is set, the exit returns `KVM_EXIT_PREEMPT` to userspace instead of re-entering;
  otherwise it is unchanged (`return 1`).

## ABI table

| Symbol | Value | File | Note |
|---|---|---|---|
| `KVM_EXIT_PREEMPT` | `42` | `include/uapi/linux/kvm.h` | Same value as the x86 0004 patch. Arch-independent exit-reason enum. |
| `KVM_ARM_PREEMPT_EXIT` | `_IO(KVMIO, 0xe4)` | `include/uapi/linux/kvm.h` | Same value as the x86 0004 patch. vcpu ioctl, arms the one-shot. |
| `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS` | `245` | `include/uapi/linux/kvm.h` | **New, arm64-only.** Next free `KVM_CAP_*` in this tree (last existing: `KVM_CAP_GUEST_MEMFD_FLAGS` = 244). Not the same cap as x86's `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` — arm64 gets its own cap number because `kvm_vm_ioctl_check_extension()`/`kvm_vm_ioctl_enable_cap()` are per-arch dispatch, but the cap namespace (the `#define`) is the shared `include/uapi/linux/kvm.h`, so it still needed a value that isn't taken by anything, x86 or arm64. |
| `KVM_ARCH_FLAG_DETERMINISTIC_INTERCEPTS` | bit `11` | `arch/arm64/include/asm/kvm_host.h`, `struct kvm_arch.flags` | Next free bit after `KVM_ARCH_FLAG_WRITABLE_IMP_ID_REGS` (bit 10). VM-level opt-in, default-off. |
| `vcpu->arch.preempt_armed` | `bool` | `arch/arm64/include/asm/kvm_host.h`, `struct kvm_vcpu_arch` | Per-vCPU one-shot. Set by `KVM_ARM_PREEMPT_EXIT`, cleared only when the kernel fires it (mirrors the x86 patch's own disarm asymmetry — see below). |

Call sequence: userspace enables `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS` once per VM
(`kvm_vm_ioctl_enable_cap`) → before each `KVM_RUN` where a force-exit is wanted, issues the
`KVM_ARM_PREEMPT_EXIT` vcpu ioctl (`kvm_arch_vcpu_ioctl`) → the next `ARM_EXCEPTION_IRQ` exit
either consumes the arm and returns `KVM_EXIT_PREEMPT`, or (if never armed, or the cap was never
enabled) returns `1` and KVM re-enters the guest as stock behavior.

## The load-bearing arch difference (residual, not a defect)

On x86 the PMI is an NMI and the exit is NMI-specific, so the exit is nearly unambiguous — an
armed vCPU exits `KVM_EXIT_PREEMPT` almost exclusively on the work-counter overflow it was armed
for. On arm64 the PMU overflow arrives as an ordinary maskable IRQ, and *every* host IRQ (the
host timer tick, in particular) leaves the guest through the same single `ARM_EXCEPTION_IRQ`
path that PMU overflow uses. **An armed vCPU therefore exits `KVM_EXIT_PREEMPT` on any host IRQ,
not only on the work-counter overflow.**

This is a **named residual, not a defect**. It costs efficiency, not correctness: every
`KVM_EXIT_PREEMPT` must be treated as advisory by userspace — re-read the work counter and, if
the deadline was not actually reached, re-arm and re-enter. This is exactly the handling the x86
backend already implements for **stale** `KVM_EXIT_PREEMPT` exits, described in the "Disarm
asymmetry" note in `consonance/vmm-backend/kvm-patches/patches/README.md`: 0004's arm is cleared
only when the NMI actually fires it — there is no disarm-on-other-exit and no disarm ioctl — so
an arm set for a `run_until` free-run can outlive an early guest exit and later surface as a
stale `KVM_EXIT_PREEMPT` on a plain `run()` that takes an unrelated NMI. `decode_exit`
(`consonance/vmm-backend/src/kvm.rs`) already swallows such stale exits as transparent
re-entries rather than treating them as unhandled. On arm64 this same swallow-and-re-enter path
becomes the **primary** handling for most `KVM_EXIT_PREEMPT` exits, not just the rare stale
case — the same code, a different frequency. The bounded-skid claim (work never overshoots
`target`) is unaffected by this; only exit *efficiency* is, and that is mitigated by core
isolation / `nohz_full` on the pinned measurement core (`docs/BOX-PINNING.md`) reducing
unrelated host-IRQ traffic on the vCPU's core.

**What stage AA-3 must evaluate, not what this patch implements:** a precise-exit alternative —
moving the work counter in-kernel with `perf_event_create_kernel_counter()` and an overflow
handler that sets a `preempt_pending` flag directly on the actual counter overflow, rather than
piggybacking on the generic IRQ exit path — would make the exit precise (armed vCPU exits only
on genuine overflow), at the cost of new counter-configuration ioctls (the counter would need to
be created/configured from in-kernel context, not perf's userspace-facing
`perf_event_open()` path). This is flagged here as the AA-3 design question. **It is not
implemented in this patch.**

## Conflict warning: applying this series alongside the x86 series

`KVM_EXIT_PREEMPT` (42) and `KVM_ARM_PREEMPT_EXIT` (`_IO(KVMIO, 0xe4)`) in this patch's
`include/uapi/linux/kvm.h` hunk are **the same two lines**, byte-for-byte, that the x86 0004
patch (`consonance/vmm-backend/kvm-patches/patches/0004-*.patch`) adds to the same file. Both
patches were generated against (functionally) the same pristine base, so:

- Applying **this** arm64 patch alone to a pristine tree: clean.
- Applying the **x86** series (0001-0005) alone to a pristine tree: clean.
- Applying **both series to the same tree** (e.g. a tree meant to build both `arch/x86/kvm` and
  `arch/arm64/kvm` — not a normal single-arch kernel build, but plausible for a shared UAPI
  header review or a cross-arch CI check): the second series's `include/uapi/linux/kvm.h` hunk
  will **not** apply cleanly, because both hunks target the same two insertion points
  (`KVM_EXIT_TDX` → `+KVM_EXIT_PREEMPT` and `KVM_HAS_DEVICE_ATTR` → `+KVM_ARM_PREEMPT_EXIT`) and
  the second patch's context will already have been inserted by the first. `git am` will reject
  the conflicting hunk; the fix is to **de-duplicate that one hunk by hand** (keep a single copy
  of the two `#define` lines, since the values are identical across both series by construction
  — this is deliberate: same UAPI, same numbers, same meaning, arch-independent enum) and drop
  it from whichever series applies second. The `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS` (245) and
  `arch/arm64/*` hunks have no such collision — they are arm64-only additions with no x86
  counterpart at these exact symbols.

## Files

- `0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch` — the patch, produced with
  `git format-patch` against tag `v6.18.35-pristine` (author `spike <s@s>`).
