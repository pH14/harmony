# Guest RDRAND cannot be intercepted on this chip

## The finding

A hypervisor on this part can freeze the CPUID model a guest sees, and that freeze is
enforced. It cannot stop the guest executing RDRAND, and a guest that executes it
receives real hardware entropy regardless of what its CPUID model says. Determinism for
guest RDRAND has to come from somewhere other than the virtualisation controls.

This is an architecture limit, not a gap in the patch series. It is separate from the
RDTSC finding in `amd-determinism-kernel-gap.md`, which is a gap in the patch series.

## The architectural difference

VMX has both controls:

    arch/x86/include/asm/vmx.h
      SECONDARY_EXEC_RDRAND_EXITING
      SECONDARY_EXEC_RDSEED_EXITING

SVM's intercept vector has neither. Searching `arch/x86/include/asm/svm.h` in the tree
that was built and booted for this program, kernel 6.18.35, returns nothing for either
instruction. The vector does carry `INTERCEPT_RDTSC` and `INTERCEPT_RDTSCP`, which is
why the RDTSC story is a software one and this one is not.

## The measurement

`ae6-rdrand` (source kept beside the record at `box/stage2/ae6-rdrand.c`) builds a KVM
guest, clears leaf 1 ECX bit 30 in the frozen CPUID model handed to the vCPU, installs
a real-mode #UD handler so that a fault and a successful execution are distinguishable
rather than one of them being a hang, and then has the guest execute RDRAND twice.

Five runs on the patched kernel, 6.18.35, the release the AMD draft contract column
names. `box/stage2/ae6-rdrand.json`, identical in every field that matters:

- host leaf 1 ECX `0xf7fa3203`, bit 30 set.
- guest leaf 1 ECX `0xb7fa3203`, bit 30 clear. The CPUID intercept enforced the frozen
  model, which is the positive control in the same run: the lever that does work, works.
- `ud_faulted` 0. No #UD was raised.
- `executed` 1, carry flag set on both instructions, so both returned a valid value.
- ten distinct 32-bit values across the five runs, the first two being `0xf94e0ab2` and
  `0x823d6140`.

So `cpuid_mask_enforced_execution` is 0: masking the feature bit changed what the guest
was told and changed nothing about what the guest could do.

## What it means for the baseline

A guest that never executes RDRAND is unaffected, and a cooperative guest built against
the frozen model will not execute it. The exposure is a guest that executes the
instruction anyway - because it was compiled for a different model, because a library
probes for entropy sources by trying them, or because the code is hostile. On this chip
that guest reads non-reproducible bits and nothing in the hypervisor sees it happen.

The honest scope statement for the AMD lane is therefore: the work clock and the landing
mechanism are the parts this program qualifies, and instruction-level entropy denial is
not available here at the virtualisation layer for RDRAND and RDSEED.

## The backend claimed otherwise

Found later, while running the machinery inside a virtual machine.
`vmm-backend`'s `patched_capabilities()` returned `deterministic_rng: true` for any host
that took the patched path, so on this chip it advertised a guarantee the silicon cannot
give. The enable path already knew better — it asks `KVM_CHECK_EXTENSION` for the
classes the host covers and requests only those, and it carries a comment saying a guest
`RDRAND` on SVM reaches the hardware unseen — and the capability report ignored the
answer.

The report now derives from the mask the host granted. Confirmed on the box before it
was spun down: the patched backend reports `rng=false tsc=true`, and `ae7-rdtsc`
independently shows the host advertising `0x5`, the time-stamp and preemption classes
only. See `rdtsc/the-time-stamp-intercepts-on-svm.md` and `nested/README.md`.

